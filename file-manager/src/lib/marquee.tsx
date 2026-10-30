import { useRef, useState, type RefObject } from "react";
import { isMac } from "../components/Chrome";

/** How far the pointer travels before a press on the empty space draws a rectangle. */
const START = 4;
/** Within this distance of the top or bottom edge, the rectangle scrolls the view. */
const EDGE = 24;

/** What a view asks of the window for its selection rectangle. */
export interface MarqueeHandlers {
  onStart: () => string[];
  onChange: (hits: string[], additive: boolean, base: string[]) => void;
}

export interface Rect {
  left: number;
  top: number;
  width: number;
  height: number;
}

/**
 * Finder's selection rectangle over the empty space of a view. Items are the
 * elements marked `data-path` inside `container` (a scroll box that is
 * positioned, so the rectangle scrolls with them). `onStart` gives the
 * selection to add to while Cmd or Shift is held; `onChange` gets the items
 * the rectangle touches as it grows; a press that never moved is `onClick`
 * (the empty space clears the selection).
 */
export function useMarquee(
  container: RefObject<HTMLElement | null>,
  handlers: {
    onStart: () => string[];
    onChange: (hits: string[], additive: boolean, base: string[]) => void;
    onClick: () => void;
    /** The items under a rectangle in content coordinates, for a view that draws only
     * the rows on screen (the rectangle scrolls past the ones not drawn). */
    hits?: (r: Rect) => string[];
  },
) {
  const [rect, setRect] = useState<Rect | null>(null);
  const latest = useRef(handlers);
  latest.current = handlers;

  const onMouseDown = (ev: React.MouseEvent) => {
    const el = container.current;
    // The left button on the empty space: an item has its own press.
    if (!el || ev.button !== 0 || (ev.target as HTMLElement).closest("[data-path]")) return;
    const box = el.getBoundingClientRect();
    // A press on the scroll bar scrolls.
    if (ev.clientX - box.left >= el.clientWidth) return;
    ev.preventDefault();
    const additive = ev.shiftKey || (isMac ? ev.metaKey : ev.ctrlKey);
    const base = additive ? latest.current.onStart() : [];
    // Content coordinates: they stay put while the view scrolls.
    const at = (x: number, y: number) => ({ x: x - box.left + el.scrollLeft, y: y - box.top + el.scrollTop });
    const start = at(ev.clientX, ev.clientY);
    let moved = false;

    const move = (m: MouseEvent) => {
      if (m.clientY > box.bottom - EDGE) el.scrollTop += 12;
      else if (m.clientY < box.top + EDGE) el.scrollTop -= 12;
      const cur = at(m.clientX, m.clientY);
      if (!moved && Math.hypot(cur.x - start.x, cur.y - start.y) < START) return;
      moved = true;
      const r = {
        left: Math.max(0, Math.min(start.x, cur.x)),
        top: Math.max(0, Math.min(start.y, cur.y)),
        width: Math.abs(cur.x - start.x),
        height: Math.abs(cur.y - start.y),
      };
      setRect(r);
      const custom = latest.current.hits;
      if (custom) {
        latest.current.onChange(custom(r), additive, base);
        return;
      }
      const hits: string[] = [];
      for (const item of el.querySelectorAll<HTMLElement>("[data-path]")) {
        const b = item.getBoundingClientRect();
        const i = at(b.left, b.top);
        if (i.x < r.left + r.width && i.x + b.width > r.left && i.y < r.top + r.height && i.y + b.height > r.top)
          hits.push(item.dataset.path!);
      }
      latest.current.onChange(hits, additive, base);
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      setRect(null);
      if (!moved && !additive) latest.current.onClick();
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  return { onMouseDown, rect };
}

/** The rectangle as drawn: in the accent, over the items. */
export function MarqueeBox({ rect }: { rect: Rect | null }) {
  if (!rect) return null;
  return <div className="marquee" style={rect} />;
}
