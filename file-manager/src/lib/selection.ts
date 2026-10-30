import type { Nav } from "./nav";

// Finder's multiple selection, as pure changes of the navigation state. The
// item lists (`order`) are the paths in the order the view shows them.

/** Cmd+click (Ctrl on Linux): adds the item, or takes it out. */
export function toggle(nav: Nav, path: string): Nav {
  if (nav.selection.includes(path)) {
    const selection = nav.selection.filter((p) => p !== path);
    const selected = nav.selected === path ? (selection.at(-1) ?? null) : nav.selected;
    return { ...nav, selection, selected, anchor: selected };
  }
  return { ...nav, selection: [...nav.selection, path], selected: path, anchor: path };
}

/**
 * Shift+click in a list or a column, and Shift+arrows: every item from the
 * anchor to `path`. The anchor stays, so the range can grow or shrink from
 * it; an anchor outside `order` starts the range at `path`.
 */
export function rangeTo(nav: Nav, path: string, order: string[]): Nav {
  const to = order.indexOf(path);
  if (to < 0) return nav;
  const found = nav.anchor == null ? -1 : order.indexOf(nav.anchor);
  const from = found < 0 ? to : found;
  const [a, b] = from <= to ? [from, to] : [to, from];
  return { ...nav, selection: order.slice(a, b + 1), selected: path, anchor: order[from] };
}

/** Cmd+A: everything in `order`; the focus stays when it is among them. */
export function selectAll(nav: Nav, order: string[]): Nav {
  const selected = nav.selected && order.includes(nav.selected) ? nav.selected : (order[0] ?? null);
  return { ...nav, selection: [...order], selected, anchor: selected };
}

/**
 * The selection rectangle: the items it touches, plus, with Cmd or Shift
 * held, what was selected when it started (`base`).
 */
export function marquee(nav: Nav, base: string[], hits: string[], additive: boolean, order: string[]): Nav {
  const chosen = new Set(additive ? [...base, ...hits] : hits);
  const selection = order.filter((p) => chosen.has(p));
  const selected = hits.at(-1) ?? (selection.includes(nav.selected ?? "") ? nav.selected : (selection.at(-1) ?? null));
  return { ...nav, selection, selected, anchor: selection[0] ?? null };
}

/** Items that are gone (moved away, deleted) leave the selection. */
export function without(nav: Nav, paths: string[]): Nav {
  const gone = new Set(paths);
  if (!nav.selection.some((p) => gone.has(p)) && !(nav.selected && gone.has(nav.selected))) return nav;
  const selection = nav.selection.filter((p) => !gone.has(p));
  const selected = nav.selected && !gone.has(nav.selected) ? nav.selected : (selection.at(-1) ?? null);
  return { ...nav, selection, selected, anchor: nav.anchor && !gone.has(nav.anchor) ? nav.anchor : selected };
}
