import { ChevronDown, ChevronUp } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import type { Entry } from "../lib/api";
import { GroupHead } from "../components/GroupHead";
import { EntryName } from "../components/NameField";
import { PathTip, usePathTips } from "../components/PathTip";
import { dropAttr, useIsDropTarget } from "../lib/dnd";
import { MarqueeBox, useMarquee, type MarqueeHandlers } from "../lib/marquee";
import { useIsCut } from "../lib/fileops";
import { dateLabel, iconFor, kindLabel } from "../lib/kinds";
import type { Group, Sort, SortKey } from "../lib/organize";
import { bytesText } from "../ui/format";
import { itemsBetween, offsetsOf, useVirtual } from "../lib/virtual";
import { t, type Key } from "../i18n";

/** The columns a narrow list drops (a pane of a split view), by its own width:
 * Kind below 42rem, Date Modified below 32rem; the name always keeps its room. */
const SHOW_MODIFIED = "hidden @lg:flex";
const SHOW_KIND = "hidden @2xl:flex";
const CELL_MODIFIED = "hidden @lg:block";
const CELL_KIND = "hidden @2xl:block";

const COLUMNS: { key: SortKey; label: Key; className: string }[] = [
  { key: "name", label: "views.col.name", className: "flex min-w-0 flex-1" },
  { key: "modified", label: "views.col.modified", className: `${SHOW_MODIFIED} w-36 flex-none` },
  { key: "size", label: "views.col.size", className: "flex w-[4.5rem] flex-none text-right" },
  { key: "kind", label: "views.col.kind", className: `${SHOW_KIND} w-32 flex-none` },
];

export function ListView({
  groups,
  sort,
  onSort,
  selection,
  focus,
  onPress,
  onOpen,
  onClear,
  marquee,
  whereRoot,
  dir,
  onContext,
  empty,
}: {
  /** The items in their sections (one untitled section when nothing is grouped). */
  groups: Group[];
  sort: Sort;
  onSort: (sort: Sort) => void;
  selection: ReadonlySet<string>;
  /** The focused row, kept in view. */
  focus: string | null;
  /** A mouse press on a row: selects it (Cmd and Shift add) and may start a drag. */
  onPress: (ev: React.MouseEvent, entry: Entry) => void;
  /** A double-click (Cmd held: a folder opens in a new tab). */
  onOpen: (entry: Entry, ev?: React.MouseEvent) => void;
  /** A click on the empty space below the rows. */
  onClear: () => void;
  marquee: MarqueeHandlers;
  /** Search results: show where each entry lives, relative to this folder
   * (and its full path on hover). */
  whereRoot?: string;
  /** The folder in view, which takes drops on the empty space (none for search results). */
  dir?: string;
  /** A right click on a row, or below the rows (`null`). */
  onContext: (ev: React.MouseEvent, entry: Entry | null) => void;
  empty: string;
}) {
  const entries = useMemo(() => groups.flatMap((g) => g.entries), [groups]);
  // The sections as one sequence of fixed-height items, drawn only where they show.
  const items = useMemo(() => flatten(groups), [groups]);
  const offsets = useMemo(() => offsetsOf(items.map((it) => SIZE[it.kind])), [items]);
  const body = useRef<HTMLDivElement | null>(null);
  const v = useVirtual(body, offsets, PAD);
  useEffect(() => {
    const i = items.findIndex((it) => it.kind === "row" && it.entry.path === focus);
    if (i >= 0) v.show(i);
    // Only when the focus moves: a re-sort keeps the scroll where it is.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus]);
  const tips = usePathTips(whereRoot != null, entries);
  const dropHere = useIsDropTarget(dir, "area");
  const rubber = useMarquee(body, {
    ...marquee,
    onClick: onClear,
    // The rows a rectangle crosses, from their places (the ones off screen are not drawn).
    hits: (r) => {
      const width = body.current?.clientWidth ?? 0;
      if (r.left >= width - ROW_INSET || r.left + r.width <= ROW_INSET) return [];
      return itemsBetween(offsets, r.top - PAD, r.top + r.height - PAD).flatMap((i) => {
        const it = items[i];
        return it.kind === "row" ? [it.entry.path] : [];
      });
    },
  });

  return (
    <div data-view="list" className="@container flex min-h-0 flex-1 flex-col">
      <div className="flex h-9 flex-none items-center gap-3 border-b border-line px-5 text-[0.7rem] font-semibold text-dim">
        {COLUMNS.filter((c) => !(whereRoot && c.key === "kind")).map((c) => {
          const active = sort.key === c.key;
          const Arrow = sort.asc ? ChevronUp : ChevronDown;
          return (
            <button
              key={c.key}
              onClick={() => onSort({ key: c.key, asc: active ? !sort.asc : true })}
              className={`items-center gap-1 ${c.className} ${c.key === "size" ? "justify-end" : ""} ${active ? "text-ink" : "hover:text-muted"}`}
            >
              {t(c.label)}
              {active && <Arrow className="h-3 w-3" />}
            </button>
          );
        })}
        {whereRoot && <span className="hidden w-40 flex-none @xl:block">{t("views.col.where")}</span>}
      </div>
      <div
        ref={body}
        className={`scroll fade-edges relative min-h-0 flex-1 px-2 py-3 ${dropHere ? "drop-zone" : ""}`}
        {...dropAttr(dir, "area")}
        // The empty space below the rows draws a selection rectangle; a click there clears the selection.
        onMouseDown={rubber.onMouseDown}
        onContextMenu={(ev) => {
          ev.preventDefault();
          onContext(ev, null);
        }}
      >
        <div style={{ height: v.before }} />
        {items.slice(v.start, v.end).map((it) => {
          if (it.kind === "head")
            return (
              <div key={it.key} className="flex flex-col justify-end" style={{ height: SIZE.head }}>
                <GroupHead label={it.label} count={it.count} className="mx-3 mb-1" />
              </div>
            );
          if (it.kind === "gap") return <div key={it.key} style={{ height: SIZE.gap }} />;
          const e = it.entry;
          const { Icon, color } = iconFor(e);
          const on = selection.has(e.path);
          return (
            <Row
              key={e.path}
              entry={e}
              {...tips.bind(e, "start")}
              data-path={e.path}
              onMouseDown={(ev) => onPress(ev, e)}
              onDoubleClick={(ev) => onOpen(e, ev)}
              onContextMenu={(ev) => {
                ev.preventDefault();
                ev.stopPropagation();
                onContext(ev, e);
              }}
              className={`flex h-8 cursor-default items-center gap-3 rounded-lg px-3 text-[0.8rem] ${
                on ? "text-paper" : `${it.striped ? "bg-white/[0.025]" : ""} hover:bg-white/5`
              }`}
              style={on ? { background: "rgb(var(--a1-rgb) / 0.32)", boxShadow: "inset 0 0 0 1px rgb(var(--a1-rgb) / 0.4)" } : undefined}
            >
              <span className="flex min-w-0 flex-1 items-center gap-2.5">
                <Icon className="h-4 w-4 flex-none" style={{ color }} strokeWidth={1.9} />
                <EntryName entry={e} className="truncate" fieldClassName="-ml-1.5 h-6 flex-1 px-1.5" />
              </span>
              <span className={`${CELL_MODIFIED} w-36 flex-none truncate ${on ? "text-paper/80" : "text-muted"}`}>{dateLabel(e.modified)}</span>
              <span className={`w-[4.5rem] flex-none text-right tabular-nums ${on ? "text-paper/80" : "text-muted"}`}>
                {e.is_dir ? "--" : bytesText(e.size)}
              </span>
              {!whereRoot && <span className={`${CELL_KIND} w-32 flex-none truncate ${on ? "text-paper/80" : "text-muted"}`}>{kindLabel(e)}</span>}
              {whereRoot && (
                <span className={`hidden w-40 flex-none truncate @xl:block ${on ? "text-paper/80" : "text-dim"}`}>{relativeParent(e.path, whereRoot)}</span>
              )}
            </Row>
          );
        })}
        <div style={{ height: v.after }} />
        {entries.length === 0 && <div className="grid h-full place-items-center text-xs text-dim">{empty}</div>}
        <MarqueeBox rect={rubber.rect} />
      </div>
      {tips.tip && whereRoot != null && <PathTip tip={tips.tip} root={whereRoot} />}
    </div>
  );
}

/** The list's top padding (`py-3`): the rows start under it. */
const PAD = 12;
/** The rows' side inset in the list (`px-2`). */
const ROW_INSET = 8;
/** Heights: a row (`h-8`), a section's heading, the space after a section. */
const SIZE = { row: 32, head: 40, gap: 8 } as const;

type Item =
  | { kind: "head"; key: string; label: string; count: number }
  | { kind: "row"; entry: Entry; striped: boolean }
  | { kind: "gap"; key: string };

/** The sections as one list: a heading, the rows (striped from each section's
 * first, as before), and some space after a titled section. */
function flatten(groups: Group[]): Item[] {
  const out: Item[] = [];
  for (const g of groups) {
    if (g.label) out.push({ kind: "head", key: `head:${g.key}`, label: g.label, count: g.entries.length });
    g.entries.forEach((entry, i) => out.push({ kind: "row", entry, striped: i % 2 === 0 }));
    if (g.label) out.push({ kind: "gap", key: `gap:${g.key}` });
  }
  return out;
}

/** A list row, faded while hidden or waiting to be moved by a Paste; a
 * folder's row takes drops. */
function Row({ entry, className, ...rest }: { entry: Entry } & React.HTMLAttributes<HTMLDivElement>) {
  const cut = useIsCut(entry.path);
  const folder = entry.is_dir ? entry.path : undefined;
  const target = useIsDropTarget(folder, "item");
  return (
    <div
      className={`${className} ${cut ? "opacity-40" : entry.hidden ? "opacity-60" : ""} ${target ? "drop-target" : ""}`}
      {...dropAttr(folder, "item")}
      {...rest}
    />
  );
}

function relativeParent(path: string, root: string): string {
  const parent = path.slice(0, path.lastIndexOf("/")) || "/";
  if (parent === root) return t("views.here");
  return parent.startsWith(root + "/") ? parent.slice(root.length + 1) : parent;
}
