/**
 * A tab of the terminal split into panes, as a tree: a leaf is one pane
 * (its key); a split puts two subtrees side by side ("row") or one above
 * the other ("column"), `ratio` being the first one's share.
 */
export type Layout = { leaf: number } | { dir: "row" | "column"; ratio: number; a: Layout; b: Layout };

/** Which side of a split, down to a node. */
export type Path = ("a" | "b")[];

export const isLeaf = (l: Layout): l is { leaf: number } => "leaf" in l;

/** The panes, in reading order (left to right, top to bottom). */
export function leaves(l: Layout): number[] {
  return isLeaf(l) ? [l.leaf] : [...leaves(l.a), ...leaves(l.b)];
}

/** `target` split in two: it keeps the first half, `key` takes the second. */
export function split(l: Layout, target: number, dir: "row" | "column", key: number): Layout {
  if (isLeaf(l)) return l.leaf === target ? { dir, ratio: 0.5, a: l, b: { leaf: key } } : l;
  return { ...l, a: split(l.a, target, dir, key), b: split(l.b, target, dir, key) };
}

/** The tree without `target`: its sibling takes the whole split. Null when it was the last pane. */
export function remove(l: Layout, target: number): Layout | null {
  if (isLeaf(l)) return l.leaf === target ? null : l;
  const a = remove(l.a, target);
  const b = remove(l.b, target);
  if (a == null) return b;
  if (b == null) return a;
  return { ...l, a, b };
}

/** The split at `path` with a new ratio (kept between 10% and 90%). */
export function setRatio(l: Layout, path: Path, ratio: number): Layout {
  if (isLeaf(l)) return l;
  if (path.length === 0) return { ...l, ratio: Math.min(0.9, Math.max(0.1, ratio)) };
  const [side, ...rest] = path;
  return side === "a" ? { ...l, a: setRatio(l.a, rest, ratio) } : { ...l, b: setRatio(l.b, rest, ratio) };
}
