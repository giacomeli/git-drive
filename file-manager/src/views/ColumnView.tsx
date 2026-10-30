import { ChevronRight, LoaderCircle } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import type { Entry } from "../lib/api";
import type { DirState } from "../lib/useDir";
import { iconFor } from "../lib/kinds";
import { dropAttr, useIsDropTarget } from "../lib/dnd";
import { useIsCut } from "../lib/fileops";
import { useColumnWidths } from "../lib/columnWidths";
import { t } from "../i18n";
import { EntryName } from "../components/NameField";
import { offsetsOf, useVirtual } from "../lib/virtual";

/** A column's top padding (`py-3`) and row height (`h-8`). */
const PAD = 12;
const ROW = 32;

/**
 * Finder's column view: one column per folder from the anchor place down to
 * the location (the preview is a panel of its own, beside the view). Each
 * column resizes by its right divider, as in Finder.
 */
export function ColumnView({
  columns,
  dirs,
  selection,
  focus,
  onPress,
  onOpen,
  onContext,
  onBackground,
}: {
  columns: string[];
  dirs: Record<string, DirState>;
  selection: ReadonlySet<string>;
  /** The focused item, kept in view. */
  focus: string | null;
  /** A mouse press on a row: selects it (Cmd and Shift add, within its column) and may start a drag. */
  onPress: (ev: React.MouseEvent, entry: Entry, columnDir: string) => void;
  /** A double-click (Cmd held: a folder opens in a new tab). */
  onOpen: (entry: Entry, ev?: React.MouseEvent) => void;
  /** A right click on a row, or on a column's empty space (`null`), with that column's folder. */
  onContext: (ev: React.MouseEvent, entry: Entry | null, dir: string) => void;
  /** A right click beyond the last column: the folder in view's menu. */
  onBackground: (ev: React.MouseEvent) => void;
}) {
  const scroller = useRef<HTMLDivElement | null>(null);
  const widths = useColumnWidths();
  const namesOf = (dir: string) => dirs[dir]?.listing?.entries.map((e) => e.name) ?? [];
  const fitAll = () => {
    for (const column of scroller.current?.querySelectorAll<HTMLElement>("[data-column-dir]") ?? []) {
      const width = fitWidth(column, namesOf(column.dataset.columnDir!));
      if (width != null) widths.setWidth(column.dataset.columnDir!, width);
    }
  };
  // Keep the deepest column in view, also once the columns finish loading.
  const loaded = columns.filter((c) => dirs[c]?.listing).length;
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const target = el.scrollWidth - el.clientWidth;
    el.scrollLeft = target;
  }, [columns.join("\n"), loaded]);

  return (
    <div
      ref={scroller}
      data-view="columns"
      className="scroll flex min-h-0 flex-1 overflow-x-auto overflow-y-hidden"
      onContextMenu={(ev) => {
        // A column answers for its own space; past the last one, the folder in view does.
        if ((ev.target as HTMLElement).closest("[data-column-dir]")) return;
        ev.preventDefault();
        onBackground(ev);
      }}
    >
      {columns.map((dir, i) => {
        const state = dirs[dir];
        const next = columns[i + 1];
        return (
          <div
            key={dir}
            data-column-dir={dir}
            className="relative flex flex-none flex-col border-r border-line"
            style={{ width: widths.widthOf(dir) }}
          >
            <ColumnList
              dir={dir}
              state={state}
              next={next}
              selection={selection}
              focus={focus}
              onPress={onPress}
              onOpen={onOpen}
              onContext={onContext}
            />
            <ResizeHandle
              width={widths.widthOf(dir)}
              onResize={(w, all) => (all ? widths.setAll(w) : widths.setWidth(dir, w))}
              onFit={(column, all) => {
                if (all) return fitAll();
                const width = fitWidth(column, namesOf(dir));
                if (width != null) widths.setWidth(dir, width);
              }}
            />
          </div>
        );
      })}
    </div>
  );
}

/**
 * A column's right divider: dragging resizes the column (with Option, every
 * column, which also sets the width of the next ones); a double-click fits
 * it to its longest name (with Option, every column to its own).
 */
function ResizeHandle({
  width,
  onResize,
  onFit,
}: {
  width: number;
  onResize: (width: number, all: boolean) => void;
  onFit: (column: HTMLElement, all: boolean) => void;
}) {
  const press = (ev: React.MouseEvent<HTMLDivElement>) => {
    if (ev.button !== 0) return;
    // Not a press on the column below: no selection, no drag of an item.
    ev.preventDefault();
    ev.stopPropagation();
    const handle = ev.currentTarget;
    const x0 = ev.clientX;
    handle.classList.add("active");
    document.body.classList.add("col-resizing");
    const move = (m: MouseEvent) => onResize(width + m.clientX - x0, m.altKey);
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      handle.classList.remove("active");
      document.body.classList.remove("col-resizing");
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={t("views.resizeColumn")}
      className="col-handle"
      onMouseDown={press}
      onDoubleClick={(ev) => {
        ev.stopPropagation();
        const column = ev.currentTarget.parentElement;
        if (column) onFit(column, ev.altKey);
      }}
    >
      <span className="col-grip" />
    </div>
  );
}

let measurer: CanvasRenderingContext2D | null = null;

/** The width that shows every one of `names` whole (the rows not drawn too),
 * measured in the font of a row on screen; null for an empty column. */
function fitWidth(column: HTMLElement, names: string[]): number | null {
  measurer ??= document.createElement("canvas").getContext("2d");
  const sample = column.querySelector<HTMLElement>("[data-col-row] .truncate");
  if (!measurer || !sample || names.length === 0) return null;
  measurer.font = getComputedStyle(sample).font;
  // What the column adds around a name: icon, padding, chevron.
  const around = column.offsetWidth - sample.clientWidth;
  const widest = Math.max(...names.map((n) => measurer!.measureText(n).width));
  return Math.ceil(around + widest) + 4;
}

/** A column's scrolling list, which takes drops into its folder: only the rows
 * on screen are drawn; the focused one, or the folder open in the next
 * column, is kept in view. */
function ColumnList({
  dir,
  state,
  next,
  selection,
  focus,
  onPress,
  onOpen,
  onContext,
}: {
  dir: string;
  state: DirState | undefined;
  /** The folder open in the next column. */
  next: string | undefined;
  selection: ReadonlySet<string>;
  focus: string | null;
  onPress: (ev: React.MouseEvent, entry: Entry, columnDir: string) => void;
  onOpen: (entry: Entry, ev?: React.MouseEvent) => void;
  onContext: (ev: React.MouseEvent, entry: Entry | null, dir: string) => void;
}) {
  const scroller = useRef<HTMLDivElement | null>(null);
  const target = useIsDropTarget(dir, "area");
  const entries = useMemo(() => state?.listing?.entries ?? [], [state?.listing]);
  const offsets = useMemo(() => offsetsOf(entries.map(() => ROW)), [entries]);
  const v = useVirtual(scroller, offsets, PAD);
  const kept = entries.some((e) => e.path === focus) ? focus : next;
  useEffect(() => {
    const i = entries.findIndex((e) => e.path === kept);
    if (i >= 0) v.show(i);
    // When the kept row changes, or the listing arrives.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kept, entries]);
  return (
    <div
      ref={scroller}
      className={`scroll fade-edges min-h-0 flex-1 px-1.5 py-3 ${target ? "drop-zone" : ""}`}
      {...dropAttr(dir, "area")}
      onContextMenu={(ev) => {
        ev.preventDefault();
        onContext(ev, null, dir);
      }}
    >
      <div style={{ height: v.before }} />
      {entries.slice(v.start, v.end).map((e) => {
        const { Icon, color } = iconFor(e);
        const focused = selection.has(e.path);
        const onPath = !focused && e.path === next;
        return (
          <ColumnRow
            key={e.path}
            entry={e}
            focused={focused}
            onPath={onPath}
            icon={<Icon className="h-4 w-4 flex-none" style={{ color }} strokeWidth={1.9} />}
            onPress={(ev) => onPress(ev, e, dir)}
            onOpen={(ev) => onOpen(e, ev)}
            onContext={(ev) => onContext(ev, e, dir)}
          />
        );
      })}
      <div style={{ height: v.after }} />
      {state?.loading && !state.listing && (
        <div className="grid h-20 place-items-center text-dim">
          <LoaderCircle className="spin h-4 w-4" />
        </div>
      )}
      {state?.error && <p className="px-3 py-3 text-[0.72rem] text-coral">{state.error}</p>}
      {state?.listing && state.listing.entries.length === 0 && <p className="px-3 py-3 text-[0.72rem] text-dim">{t("views.emptyFolder")}</p>}
    </div>
  );
}

function ColumnRow({
  entry,
  focused,
  onPath,
  icon,
  onPress,
  onOpen,
  onContext,
}: {
  entry: Entry;
  /** Selected (drawn in the accent). */
  focused: boolean;
  onPath: boolean;
  icon: React.ReactNode;
  onPress: (ev: React.MouseEvent) => void;
  onOpen: (ev: React.MouseEvent) => void;
  onContext: (ev: React.MouseEvent) => void;
}) {
  const cut = useIsCut(entry.path);
  const folder = entry.is_dir ? entry.path : undefined;
  const target = useIsDropTarget(folder, "item");
  return (
    <div
      data-col-row
      data-path={entry.path}
      {...dropAttr(folder, "item")}
      onMouseDown={onPress}
      onDoubleClick={onOpen}
      onContextMenu={(ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        onContext(ev);
      }}
      className={`flex h-8 cursor-default items-center gap-2.5 rounded-lg px-2.5 text-[0.8rem] ${
        focused ? "text-paper" : onPath ? "bg-white/10 text-ink" : "hover:bg-white/5"
      } ${cut ? "opacity-40" : entry.hidden ? "opacity-60" : ""} ${target ? "drop-target" : ""}`}
      style={focused ? { background: "rgb(var(--a1-rgb) / 0.32)", boxShadow: "inset 0 0 0 1px rgb(var(--a1-rgb) / 0.4)" } : undefined}
    >
      {icon}
      <EntryName entry={entry} className="min-w-0 flex-1 truncate" fieldClassName="-ml-1.5 h-6 flex-1 px-1.5" />
      {entry.is_dir && <ChevronRight className={`h-3.5 w-3.5 flex-none ${focused ? "text-paper/80" : "text-dim"}`} />}
    </div>
  );
}
