import { parentOf, type Place } from "./api";
import { t } from "../i18n";

/** The Computer view's location: no folder, the volumes and the home's folders. */
export const COMPUTER = "computer:";

/**
 * Navigation shared by the views. `location` is the folder in view (icon and
 * list views) or the folder of the last column (column view). `selected` is
 * the focused item, the last one clicked: the arrows move from it and the
 * preview shows it; in column view it can be `location` itself (a folder
 * whose contents fill the last column). `selection` holds every selected
 * item, the focused one included; `anchor` is where a Shift+click range
 * starts (see selection.ts).
 */
export interface Nav {
  location: string;
  selected: string | null;
  selection: string[];
  anchor: string | null;
  back: string[];
  forward: string[];
}

/** One item selected (or none): focused, and the anchor of the next range. */
export const only = (path: string | null) => ({ selected: path, selection: path ? [path] : [], anchor: path });

export const startNav = (location: string): Nav => ({ location, ...only(null), back: [], forward: [] });

/** Goes to `location` and records the move in the history. */
export function go(nav: Nav, location: string, selected: string | null = null): Nav {
  if (location === nav.location) return { ...nav, ...only(selected) };
  return { location, ...only(selected), back: [...nav.back, nav.location].slice(-100), forward: [] };
}

/** Changes the location without a history entry (column clicks). */
export function moveTo(nav: Nav, location: string, selected: string | null): Nav {
  return { ...nav, location, ...only(selected) };
}

export function goBack(nav: Nav): Nav {
  const prev = nav.back[nav.back.length - 1];
  if (prev == null) return nav;
  return { location: prev, ...only(null), back: nav.back.slice(0, -1), forward: [nav.location, ...nav.forward] };
}

export function goForward(nav: Nav): Nav {
  const next = nav.forward[0];
  if (next == null) return nav;
  return { location: next, ...only(null), back: [...nav.back, nav.location], forward: nav.forward.slice(1) };
}

/** Up one level, keeping the folder we came from selected (as Finder does);
 * above the root is the Computer view, and nothing is above that. */
export function goUp(nav: Nav): Nav {
  if (nav.location === COMPUTER) return nav;
  const parent = parentOf(nav.location);
  return parent == null ? go(nav, COMPUTER) : go(nav, parent, nav.location);
}

/** `path` after `from` was renamed to `to` (itself, or anything inside it). */
export function rebase(path: string, from: string, to: string): string {
  if (path === from) return to;
  return path.startsWith(`${from}/`) ? to + path.slice(from.length) : path;
}

/** The navigation after a rename: the location, the selection and the history follow it. */
export function rebaseNav(nav: Nav, from: string, to: string): Nav {
  const r = (p: string) => rebase(p, from, to);
  return {
    location: r(nav.location),
    selected: nav.selected && r(nav.selected),
    selection: nav.selection.map(r),
    anchor: nav.anchor && r(nav.anchor),
    back: nav.back.map(r),
    forward: nav.forward.map(r),
  };
}

export const isWithin = (path: string, root: string) => root === "/" || path === root || path.startsWith(root + "/");

/** The deepest sidebar place that contains `path` ("/" when none). */
export function anchorOf(path: string, places: Place[]): Place | null {
  let best: Place | null = null;
  for (const p of places) {
    if (isWithin(path, p.path) && (!best || p.path.length > best.path.length)) best = p;
  }
  return best;
}

/**
 * The sidebar item the location belongs to: `computer`, `trash`, or the
 * deepest bookmark (`b:path`) or volume (`v:path`) that contains it; a
 * bookmark wins a tie with a volume (the drive shows as both).
 */
export function activeItem(location: string, bookmarks: string[], volumes: string[], trash: string | null): string | null {
  if (location === COMPUTER) return "computer";
  if (trash && isWithin(location, trash)) return "trash";
  const deepest = (paths: string[]) =>
    paths.filter((p) => isWithin(location, p)).reduce<string | null>((a, p) => (a == null || p.length > a.length ? p : a), null);
  const b = deepest(bookmarks);
  const v = deepest(volumes);
  if (b != null && (v == null || b.length >= v.length)) return `b:${b}`;
  return v == null ? null : `v:${v}`;
}

/** Folders shown as columns: from the anchor place down to `location`. */
export function columnsFor(location: string, places: Place[]): string[] {
  if (location === COMPUTER) return [];
  const anchor = anchorOf(location, places)?.path ?? "/";
  const chain: string[] = [];
  let cur: string | null = location;
  while (cur != null) {
    chain.unshift(cur);
    if (cur === anchor) break;
    cur = parentOf(cur);
  }
  return chain;
}

/** Breadcrumb segments: the anchor place, then each folder below it. */
export function crumbsFor(location: string, places: Place[]): { label: string; path: string }[] {
  if (location === COMPUTER) return [{ label: t("files.computer"), path: COMPUTER }];
  const anchor = anchorOf(location, places);
  const cols = columnsFor(location, places);
  return cols.map((path, i) => ({
    label: i === 0 && anchor ? anchor.label : path === "/" ? t("files.computer") : (path.split("/").pop() ?? path),
    path,
  }));
}

/** The icon view's arrow keys: Left and Right step through the items, Up and
 * Down move a row (to the last icon when the row there is shorter, as
 * Finder does). `sizes` are the lengths of the groups in order, each of
 * which starts a row of its own. `null` keeps the selection where it is. */
export function gridMove(idx: number, sizes: number[], columns: number, key: string): number | null {
  const count = sizes.reduce((a, b) => a + b, 0);
  if (count === 0) return null;
  if (idx < 0) return key === "ArrowUp" || key === "ArrowLeft" ? count - 1 : 0;
  // Rows as [first index, length].
  const rows: [number, number][] = [];
  let start = 0;
  for (const size of sizes) {
    for (let i = 0; i < size; i += columns) rows.push([start + i, Math.min(columns, size - i)]);
    start += size;
  }
  const r = rows.findIndex(([first, len]) => idx >= first && idx < first + len);
  const col = idx - rows[r][0];
  const at = (row: [number, number] | undefined) => (row ? row[0] + Math.min(col, row[1] - 1) : null);
  switch (key) {
    case "ArrowLeft":
      return Math.max(0, idx - 1);
    case "ArrowRight":
      return Math.min(count - 1, idx + 1);
    case "ArrowUp":
      return at(rows[r - 1]);
    case "ArrowDown":
      return at(rows[r + 1]);
    default:
      return null;
  }
}
