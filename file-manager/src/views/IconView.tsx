import { Grid2x2, Play } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Entry } from "../lib/api";
import type { Group } from "../lib/organize";
import { EntryGlyph } from "../components/Glyph";
import { GroupHead } from "../components/GroupHead";
import { EntryName } from "../components/NameField";
import { PathTip, usePathTips } from "../components/PathTip";
import { t } from "../i18n";
import { dropAttr, useIsDropTarget } from "../lib/dnd";
import { MarqueeBox, useMarquee, type MarqueeHandlers } from "../lib/marquee";
import { useIsCut } from "../lib/fileops";
import { ownSvg, useThumbnail } from "../lib/thumbs";
import { useFolderPreview } from "../lib/useFolderPreview";
import { itemsBetween, offsetsOf, useVirtual } from "../lib/virtual";

export const ICON_SIZE = { min: 48, max: 192, step: 8, initial: 72 };

const GRID_PAD_X = 16;
/** A tile is wider than its icon so a name fits on two lines. */
const tileWidth = (iconSize: number) => Math.max(96, iconSize + 40);
/** A tile's height, the same for all so the grid can be drawn by rows: its
 * padding, the icon's box, the gap and two lines of name. */
const tileHeight = (iconSize: number) => iconSize + 62;
/** Space between rows, before a section's first row and after its last; a section's heading. */
const ROW_GAP = 8;
const SECTION_TOP = 16;
const SECTION_END = 8;
const HEAD = 40;

type Item =
  | { kind: "head"; key: string; label: string; count: number }
  | { kind: "space"; key: string; height: number }
  | { kind: "row"; key: string; entries: Entry[] };

/** The sections as rows of `columns` tiles, with their headings and spacing. */
function rowsOf(groups: Group[], columns: number): Item[] {
  const out: Item[] = [];
  for (const g of groups) {
    if (g.label) out.push({ kind: "head", key: `head:${g.key}`, label: g.label, count: g.entries.length });
    out.push({ kind: "space", key: `top:${g.key}`, height: SECTION_TOP });
    for (let i = 0; i < g.entries.length; i += columns) out.push({ kind: "row", key: `row:${g.key}:${i}`, entries: g.entries.slice(i, i + columns) });
    out.push({ kind: "space", key: `end:${g.key}`, height: SECTION_END });
  }
  return out;
}

/**
 * Finder's icon view: a grid of large icons (thumbnails of images, videos and
 * documents once drawn) with the name below. The grid reports how many
 * columns fit, which is how far Up and Down move the selection. Search
 * results show their full path on hover.
 */
export function IconView({
  groups,
  iconSize,
  selection,
  focus,
  onPress,
  onOpen,
  onClear,
  marquee,
  onColumns,
  searchRoot,
  dir,
  onContext,
  empty,
}: {
  /** The items in their sections; each section starts a row of its own. */
  groups: Group[];
  iconSize: number;
  selection: ReadonlySet<string>;
  /** The focused item, kept in view. */
  focus: string | null;
  /** A mouse press on an icon: selects it (Cmd and Shift add) and may start a drag. */
  onPress: (ev: React.MouseEvent, entry: Entry) => void;
  /** A double-click (Cmd held: a folder opens in a new tab). */
  onOpen: (entry: Entry, ev?: React.MouseEvent) => void;
  /** A click on the empty space between icons. */
  onClear: () => void;
  marquee: MarqueeHandlers;
  onColumns: (columns: number) => void;
  /** Search results: the folder searched, for the path popover. */
  searchRoot?: string;
  /** The folder in view, which takes drops on the empty space (none for search results). */
  dir?: string;
  /** A right click on an icon, or on the empty space (`null`). */
  onContext: (ev: React.MouseEvent, entry: Entry | null) => void;
  empty: string;
}) {
  const entries = useMemo(() => groups.flatMap((g) => g.entries), [groups]);
  const scroller = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    setWidth(el.clientWidth);
    return () => ro.disconnect();
  }, []);
  const columns = Math.max(1, Math.floor((width - 2 * GRID_PAD_X) / tileWidth(iconSize)));
  useEffect(() => {
    onColumns(columns);
  }, [columns, onColumns]);
  const tips = usePathTips(searchRoot != null, entries);
  // Drawn by rows, only those on screen.
  const items = useMemo(() => rowsOf(groups, columns), [groups, columns]);
  const rowHeight = tileHeight(iconSize) + ROW_GAP;
  const offsets = useMemo(
    () => offsetsOf(items.map((it) => (it.kind === "head" ? HEAD : it.kind === "space" ? it.height : rowHeight))),
    [items, rowHeight],
  );
  const v = useVirtual(scroller, offsets);
  useEffect(() => {
    const i = items.findIndex((it) => it.kind === "row" && it.entries.some((e) => e.path === focus));
    if (i >= 0) v.show(i);
    // Only when the focus moves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus]);
  const dropHere = useIsDropTarget(dir, "area");
  const rubber = useMarquee(scroller, {
    ...marquee,
    onClick: onClear,
    // The tiles a rectangle crosses, from their places (rows off screen are not drawn).
    hits: (r) => {
      const cell = (width - 2 * GRID_PAD_X) / columns;
      const tile = tileHeight(iconSize);
      return itemsBetween(offsets, r.top, r.top + r.height).flatMap((i) => {
        const it = items[i];
        if (it.kind !== "row" || r.top >= offsets[i] + tile) return [];
        return it.entries.filter((_, c) => {
          const x = GRID_PAD_X + c * cell;
          return r.left < x + cell && r.left + r.width > x;
        }).map((e) => e.path);
      });
    },
  });

  return (
    <div
      ref={scroller}
      data-view="icons"
      className={`scroll fade-edges relative min-h-0 flex-1 ${dropHere ? "drop-zone" : ""}`}
      {...dropAttr(dir, "area")}
      // The empty space draws a selection rectangle; a click there clears the selection.
      onMouseDown={rubber.onMouseDown}
      onContextMenu={(ev) => {
        ev.preventDefault();
        onContext(ev, null);
      }}
    >
      {entries.length === 0 ? (
        <div className="grid h-full place-items-center text-xs text-dim">{empty}</div>
      ) : (
        <>
          <div style={{ height: v.before }} />
          {items.slice(v.start, v.end).map((it) =>
            it.kind === "head" ? (
              <div key={it.key} className="flex flex-col justify-end" style={{ height: HEAD, paddingLeft: GRID_PAD_X, paddingRight: GRID_PAD_X }}>
                <GroupHead label={it.label} count={it.count} className="mx-1" />
              </div>
            ) : it.kind === "space" ? (
              <div key={it.key} style={{ height: it.height }} />
            ) : (
              <div
                key={it.key}
                className="grid items-start"
                style={{ height: rowHeight, gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`, paddingLeft: GRID_PAD_X, paddingRight: GRID_PAD_X }}
              >
                {it.entries.map((e) => (
                  <Tile
                    key={e.path}
                    entry={e}
                    iconSize={iconSize}
                    selected={selection.has(e.path)}
                    onPress={onPress}
                    onOpen={onOpen}
                    hover={tips.bind(e)}
                    titled={searchRoot == null}
                    onContext={onContext}
                  />
                ))}
              </div>
            ),
          )}
          <div style={{ height: v.after }} />
        </>
      )}
      <MarqueeBox rect={rubber.rect} />
      {tips.tip && searchRoot != null && <PathTip tip={tips.tip} root={searchRoot} />}
    </div>
  );
}

function Tile({
  entry,
  iconSize,
  selected,
  onPress,
  onOpen,
  hover,
  titled,
  onContext,
}: {
  entry: Entry;
  iconSize: number;
  selected: boolean;
  onPress: (ev: React.MouseEvent, entry: Entry) => void;
  onOpen: (entry: Entry, ev?: React.MouseEvent) => void;
  /** The path popover's handlers (search results only). */
  hover: ReturnType<ReturnType<typeof usePathTips>["bind"]>;
  /** The native tooltip with the name; off where the path popover shows. */
  titled: boolean;
  onContext: (ev: React.MouseEvent, entry: Entry) => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const cut = useIsCut(entry.path);
  const folder = entry.is_dir ? entry.path : undefined;
  const target = useIsDropTarget(folder, "item");
  const thumb = useThumbnail(entry, iconSize, ref);
  const previews = useFolderPreview(entry, iconSize, ref);
  const box = iconSize + 12;
  return (
    <div
      ref={ref}
      title={titled ? entry.name : undefined}
      {...hover}
      {...dropAttr(folder, "item")}
      data-path={entry.path}
      onMouseDown={(ev) => {
        ev.stopPropagation();
        onPress(ev, entry);
      }}
      onDoubleClick={(ev) => onOpen(entry, ev)}
      onContextMenu={(ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        onContext(ev, entry);
      }}
      className={`flex cursor-default flex-col items-center gap-1.5 px-1 py-1 ${cut ? "opacity-40" : entry.hidden ? "opacity-60" : ""}`}
      // The same height for every tile: the grid is drawn by rows. A name being edited may grow past it.
      style={{ height: tileHeight(iconSize) }}
    >
      <div
        className={`grid place-items-center rounded-[12px] transition-colors ${target ? "drop-target" : selected ? "bg-white/12" : ""}`}
        style={{ width: box, height: box }}
      >
        {thumb ? <Thumb url={thumb} entry={entry} size={iconSize} /> : <EntryGlyph entry={entry} size={iconSize} prints={previews.prints} fresh={previews.fresh} />}
      </div>
      <EntryName
        entry={entry}
        className={`line-clamp-2 max-w-full rounded-md px-1.5 text-center text-[0.74rem] leading-[1.1rem] [overflow-wrap:anywhere] ${
          selected ? "text-paper" : "text-ink/90"
        }`}
        style={{
          maxWidth: tileWidth(iconSize) - 8,
          ...(selected ? { background: "rgb(var(--a1-rgb) / 0.6)", boxShadow: "inset 0 0 0 1px rgb(var(--a1-rgb) / 0.5)" } : {}),
        }}
        multiline
        fieldClassName="px-1.5 py-px text-center text-[0.74rem] leading-[1.1rem]"
        fieldStyle={{ width: tileWidth(iconSize) - 8 }}
      />
    </div>
  );
}

/** A drawn thumbnail: framed like a photo print (SVG floats, as artwork);
 * videos get a play mark. */
function Thumb({ url, entry, size }: { url: string; entry: Entry; size: number }) {
  const video = ["mp4", "m4v", "mov"].includes(entry.ext);
  const framed = !ownSvg(entry);
  return (
    <div className="fade-in relative flex items-center justify-center" style={{ width: size, height: size }}>
      {/* Pixel bounds: a percentage of an auto-sized box shrank the image to a dot. */}
      <img
        src={url}
        alt=""
        draggable={false}
        className={`object-contain ${framed ? "rounded-[4px] shadow-[0_6px_16px_-6px_rgb(0_0_0/0.7)] ring-1 ring-white/20" : ""}`}
        style={framed ? { maxWidth: size, maxHeight: size } : { width: size, height: size }}
      />
      {video && (
        <span
          className="absolute grid place-items-center rounded-full bg-black/50 ring-1 ring-white/30"
          style={{ width: size * 0.32, height: size * 0.32 }}
        >
          <Play
            className="text-paper"
            style={{ width: size * 0.14, height: size * 0.14, marginLeft: size * 0.015 }}
            fill="currentColor"
            strokeWidth={0}
          />
        </span>
      )}
    </div>
  );
}

/** Icon size, Finder's slider in the status bar. */
export function IconSizeSlider({ value, onChange }: { value: number; onChange: (v: number) => void }) {
  const fill = ((value - ICON_SIZE.min) / (ICON_SIZE.max - ICON_SIZE.min)) * 100;
  return (
    <span className="flex items-center gap-2">
      <Grid2x2 className="h-2.5 w-2.5 text-dim" />
      <input
        type="range"
        aria-label={t("views.iconSize")}
        className="range w-24"
        min={ICON_SIZE.min}
        max={ICON_SIZE.max}
        step={ICON_SIZE.step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        style={{ ["--fill" as string]: `${fill}%` }}
      />
      <Grid2x2 className="h-3.5 w-3.5 text-dim" />
    </span>
  );
}
