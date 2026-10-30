// How the panes of a split tab share its width: one fraction per pane,
// summing to 1. Pure, tested in Node.

/** The most panes a tab shows side by side. */
export const MAX_PANES = 4;

/** Even shares for `n` panes. */
export const evenShares = (n: number): number[] => Array.from({ length: n }, () => 1 / n);

const sum = (xs: number[]) => xs.reduce((a, b) => a + b, 0);

/** `shares` scaled to sum 1. */
export function normalize(shares: number[]): number[] {
  const total = sum(shares);
  return total > 0 ? shares.map((s) => s / total) : evenShares(shares.length);
}

/** Whether `n` panes of at least `minPane` px fit in `width` px with a
 * `divider` px wide divider between each two. */
export function fits(n: number, width: number, minPane: number, divider: number): boolean {
  return n * minPane + (n - 1) * divider <= width + 0.5;
}

/** Every share raised to at least `least`, what is added taken from the
 * others in proportion to what they have above it. Even shares when they
 * cannot all reach it. */
export function atLeast(shares: number[], least: number): number[] {
  const s = normalize(shares);
  if (least * s.length > 1) return evenShares(s.length);
  const short = s.reduce((n, x) => n + Math.max(0, least - x), 0);
  if (short <= 0) return s;
  const spare = s.reduce((n, x) => n + Math.max(0, x - least), 0);
  return s.map((x) => (x <= least ? least : x - ((x - least) / spare) * short));
}

/** Pane `i` closed: its share goes half to each neighbor (all of it to the only one at an edge). */
export function removeAt(shares: number[], i: number): number[] {
  const s = normalize(shares);
  if (s.length <= 1) return [1];
  const gone = s[i];
  const out = s.slice();
  const left = i - 1;
  const right = i + 1;
  if (left >= 0 && right < s.length) {
    out[left] += gone / 2;
    out[right] += gone / 2;
  } else if (left >= 0) out[left] += gone;
  else out[right] += gone;
  out.splice(i, 1);
  return normalize(out);
}

/** The divider after pane `i` dragged to `x` (a fraction of the panes'
 * width, from the left): only panes `i` and `i + 1` change, each keeping
 * at least `least`. */
export function dragDivider(shares: number[], i: number, x: number, least: number): number[] {
  const s = normalize(shares);
  const start = sum(s.slice(0, i));
  const pair = s[i] + s[i + 1];
  const lo = Math.min(least, pair / 2);
  const left = Math.min(pair - lo, Math.max(lo, x - start));
  const out = s.slice();
  out[i] = left;
  out[i + 1] = pair - left;
  return out;
}
