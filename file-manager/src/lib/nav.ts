import { parentOf, type Place } from "./api";

/**
 * Navigation shared by both views. `location` is the folder in view (list
 * view) or the folder of the last column (column view). `selected` is an
 * entry of `location`, or, in column view, `location` itself (a folder whose
 * contents fill the last column).
 */
export interface Nav {
  location: string;
  selected: string | null;
  back: string[];
  forward: string[];
}

export const startNav = (location: string): Nav => ({ location, selected: null, back: [], forward: [] });

/** Goes to `location` and records the move in the history. */
export function go(nav: Nav, location: string, selected: string | null = null): Nav {
  if (location === nav.location) return { ...nav, selected };
  return { location, selected, back: [...nav.back, nav.location].slice(-100), forward: [] };
}

/** Changes the location without a history entry (column clicks). */
export function moveTo(nav: Nav, location: string, selected: string | null): Nav {
  return { ...nav, location, selected };
}

export function goBack(nav: Nav): Nav {
  const prev = nav.back[nav.back.length - 1];
  if (prev == null) return nav;
  return { location: prev, selected: null, back: nav.back.slice(0, -1), forward: [nav.location, ...nav.forward] };
}

export function goForward(nav: Nav): Nav {
  const next = nav.forward[0];
  if (next == null) return nav;
  return { location: next, selected: null, back: [...nav.back, nav.location], forward: nav.forward.slice(1) };
}

/** Up one level, keeping the folder we came from selected (as Finder does). */
export function goUp(nav: Nav): Nav {
  const parent = parentOf(nav.location);
  return parent == null ? nav : go(nav, parent, nav.location);
}

const isWithin = (path: string, root: string) => root === "/" || path === root || path.startsWith(root + "/");

/** The deepest sidebar place that contains `path` ("/" when none). */
export function anchorOf(path: string, places: Place[]): Place | null {
  let best: Place | null = null;
  for (const p of places) {
    if (isWithin(path, p.path) && (!best || p.path.length > best.path.length)) best = p;
  }
  return best;
}

/** Folders shown as columns: from the anchor place down to `location`. */
export function columnsFor(location: string, places: Place[]): string[] {
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
  const anchor = anchorOf(location, places);
  const cols = columnsFor(location, places);
  return cols.map((path, i) => ({
    label: i === 0 && anchor ? anchor.label : path === "/" ? "Computer" : path.split("/").pop() ?? path,
    path,
  }));
}
