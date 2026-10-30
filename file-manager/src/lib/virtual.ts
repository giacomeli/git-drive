import { useCallback, useLayoutEffect, useReducer, useRef, type RefObject } from "react";

/** How far beyond the visible part a list keeps its rows drawn, in pixels, above and below. */
export const OVERSCAN = 480;
/** The faded band at a list's top and bottom edges (`fade-edges` in files.css). */
const FADE = 12;

/** Where each item starts, from the sizes of all of them; the last entry is the whole height. */
export function offsetsOf(sizes: number[]): number[] {
  const out = new Array<number>(sizes.length + 1);
  out[0] = 0;
  for (let i = 0; i < sizes.length; i++) out[i + 1] = out[i] + sizes[i];
  return out;
}

/** The index of the item at `y` (the first one when above, the last when below). */
export function indexAt(offsets: number[], y: number): number {
  const n = offsets.length - 1;
  if (n <= 0) return 0;
  let lo = 0;
  let hi = n - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (offsets[mid] <= y) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}

/** The items drawn for a view of `height` pixels scrolled to `top`: [start, end). */
export function visibleRange(offsets: number[], top: number, height: number, overscan = OVERSCAN): { start: number; end: number } {
  const n = offsets.length - 1;
  if (n <= 0) return { start: 0, end: 0 };
  const start = indexAt(offsets, Math.max(0, top - overscan));
  const end = Math.min(n, indexAt(offsets, top + height + overscan) + 1);
  return { start, end };
}

/** The scroll position that shows item `i` whole, or null when it already shows (a `margin` of room kept). */
export function scrollToShow(offsets: number[], i: number, top: number, height: number, margin = 6): number | null {
  if (i < 0 || i >= offsets.length - 1) return null;
  const from = offsets[i];
  const to = offsets[i + 1];
  if (from < top) return Math.max(0, from - margin);
  if (to > top + height) return to - height + margin;
  return null;
}

/** The items a band of content [y0, y1) touches. */
export function itemsBetween(offsets: number[], y0: number, y1: number): number[] {
  const n = offsets.length - 1;
  if (n <= 0 || y1 <= 0 || y0 >= offsets[n]) return [];
  const out: number[] = [];
  for (let i = indexAt(offsets, y0); i < n && offsets[i] < y1; i++) if (offsets[i + 1] > y0) out.push(i);
  return out;
}

/**
 * A long list drawn only where it shows: the items on screen plus
 * `OVERSCAN` pixels above and below, between two spacers that keep the
 * scroll height whole. `pad` is the scroller's own top padding (the items
 * start under it). Re-renders only when the range drawn changes.
 */
export function useVirtual(scroller: RefObject<HTMLElement | null>, offsets: number[], pad = 0) {
  const latest = useRef({ offsets, pad });
  latest.current = { offsets, pad };
  // Where the scroller is: read on scroll and resize, without a render each time.
  const pos = useRef({ top: 0, height: 0 });
  const [, redraw] = useReducer((x: number) => x + 1, 0);
  const range = visibleRange(offsets, pos.current.top - pad, pos.current.height || 1200);
  const drawn = useRef(range);
  drawn.current = range;

  /** Where the scroller is now; a redraw when that changes the rows drawn. */
  const read = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    pos.current = { top: el.scrollTop, height: el.clientHeight };
    const { offsets: o, pad: p } = latest.current;
    const next = visibleRange(o, el.scrollTop - p, el.clientHeight);
    // A scroll within the rows already drawn changes nothing.
    if (next.start !== drawn.current.start || next.end !== drawn.current.end) redraw();
  }, [scroller]);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    read();
    el.addEventListener("scroll", read, { passive: true });
    const ro = new ResizeObserver(read);
    ro.observe(el);
    return () => {
      el.removeEventListener("scroll", read);
      ro.disconnect();
    };
  }, [scroller, read]);

  /** Scrolls so item `i` shows whole (the keyboard's focus), drawn at once. */
  const show = useCallback(
    (i: number) => {
      const el = scroller.current;
      if (!el) return;
      const { offsets: o, pad: p } = latest.current;
      // Clear of the faded edges (`fade-edges`).
      const to = scrollToShow(o, i, el.scrollTop - p + FADE, el.clientHeight - 2 * FADE);
      if (to != null) {
        el.scrollTop = to + p - FADE;
        read();
      }
    },
    [scroller, read],
  );

  const n = offsets.length - 1;
  return { start: range.start, end: range.end, before: offsets[range.start] ?? 0, after: (offsets[n] ?? 0) - (offsets[range.end] ?? 0), show };
}
