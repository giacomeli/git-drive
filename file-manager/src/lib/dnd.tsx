import { getCurrentWebview } from "@tauri-apps/api/webview";
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { isMac } from "../components/Chrome";
import { api, errorText, type Entry } from "./api";

/** How far the pointer travels with the button down before a press becomes a drag. */
const DRAG_THRESHOLD = 5;

/** What takes a drop: a folder's row or tile, the empty space of a folder
 * (the view's or a column's), a sidebar place, a gap between bookmarks (a
 * new bookmark), the Trash, the terminal (which types the paths), or a tab
 * (its folder; for a tab, `dir` is the tab's id). One folder can show as several of them at
 * once; only the one under the pointer lights up. */
export type DropKind = "item" | "area" | "place" | "bookmark" | "trash" | "terminal" | "tab";

/** Where a drop lands: a folder, or for a bookmark gap the index the new bookmark takes. */
export interface DropSpot {
  dir: string;
  kind: DropKind;
}

/** Marks an element as taking drops into `dir`. */
export const dropAttr = (dir: string | undefined, kind: DropKind) => (dir ? { "data-drop-dir": dir, "data-drop-kind": kind } : {});

/** Over the bookmarks: the middle of a bookmark drops into its folder; its
 * edges, and the space around the list, add a bookmark there (as Finder's
 * sidebar does). Rows are marked `data-bookmark-index`; a missing folder
 * takes no drops. */
function bookmarkSpotAt(list: HTMLElement, y: number): DropSpot {
  const rows = [...list.querySelectorAll<HTMLElement>("[data-bookmark-index]")];
  for (const [i, row] of rows.entries()) {
    const b = row.getBoundingClientRect();
    if (y < b.top || y >= b.bottom) continue;
    const edge = b.height / 4;
    const dir = row.dataset.dropDir;
    if (dir && y >= b.top + edge && y < b.bottom - edge) return { dir, kind: "place" };
    return { dir: String(y < b.top + b.height / 2 ? i : i + 1), kind: "bookmark" };
  }
  return { dir: String(rows.filter((r) => r.getBoundingClientRect().top + r.offsetHeight / 2 < y).length), kind: "bookmark" };
}

/** The drop spot under the point, from the nearest element that takes drops. */
export function dropSpotAt(x: number, y: number): DropSpot | null {
  const hit = document.elementFromPoint(x, y);
  const list = hit?.closest<HTMLElement>("[data-bookmarks]");
  if (list) return bookmarkSpotAt(list, y);
  const el = hit?.closest<HTMLElement>("[data-drop-dir]");
  const dir = el?.dataset.dropDir;
  return el && dir ? { dir, kind: (el.dataset.dropKind as DropKind | undefined) ?? "area" } : null;
}

/** How a drop spot is named in `target` (the sidebar compares against it). */
export const spotKey = (kind: DropKind, dir: string) => `${kind}:${dir}`;

/** Whether `spot` can take `paths`: a folder never goes into itself, nor below it. */
export function canDropInto(spot: DropSpot, paths: string[]): boolean {
  if (spot.kind === "bookmark" || spot.kind === "trash" || spot.kind === "terminal" || spot.kind === "tab") return true;
  return !paths.some((p) => spot.dir === p || spot.dir.startsWith(`${p}/`));
}

/** What a drag from an item carries: its paths, the reason it can't move, or nothing (a name being edited). */
export type DragPaths = string[] | { refused: string } | null;

export interface Dnd {
  /** The drop spot a drag is over (`kind:dir`), drawn highlighted. */
  target: string | null;
  /** A mouse press on an item, which becomes a drag once the pointer moves; the
   * pane that saw the press says what the drag carries (read as it starts) and
   * hears when it starts. */
  press: (ev: React.MouseEvent, entry: Entry, paths: () => DragPaths, onStart?: () => void) => void;
}

const DndContext = createContext<Dnd>({ target: null, press: () => {} });
export const DndProvider = DndContext.Provider;
export function useIsDropTarget(dir: string | undefined, kind: DropKind): boolean {
  const target = useContext(DndContext).target;
  return dir != null && target === spotKey(kind, dir);
}

/**
 * Drag and drop. A drag from here is a native one (`start_drag`), so other
 * apps take it like a drag of their own; a drop on this window, from here or
 * from another app, arrives as the webview's drop event, and lands in the
 * folder under the pointer.
 */
export function useDragAndDrop(handlers: {
  /** A drag of an item that stays where it is: say why. */
  onRefused: (reason: string) => void;
  /** Items dropped on a spot of this window. */
  onDrop: (paths: string[], spot: DropSpot) => void;
  /** A drag from here ended in another app, which may have moved the items. */
  onDraggedAway: (paths: string[]) => void;
  onError: (text: string) => void;
}): Dnd {
  const [target, setTarget] = useState<string | null>(null);
  // The listener is registered once; it reads the latest handlers.
  const latest = useRef(handlers);
  latest.current = handlers;
  // Paths of the drag over the window (only `enter` carries them).
  const incoming = useRef<string[]>([]);
  // Set when a drag from here was dropped on this window, which already handled it.
  const droppedHere = useRef(false);

  useEffect(() => {
    // Tauri types the position as physical, but on macOS (points, from
    // AppKit) and Linux (GTK's logical pixels) it is already in CSS pixels
    // from the webview's corner: scaling it would miss on a Retina screen.
    const spotAt = (p: { x: number; y: number }) => {
      const spot = dropSpotAt(p.x, p.y);
      return spot && canDropInto(spot, incoming.current) ? spot : null;
    };
    const keyAt = (p: { x: number; y: number }) => {
      const spot = spotAt(p);
      return spot && spotKey(spot.kind, spot.dir);
    };
    let alive = true;
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent(({ payload: e }) => {
        if (e.type === "enter") {
          incoming.current = e.paths;
          setTarget(keyAt(e.position));
        } else if (e.type === "over") {
          setTarget(keyAt(e.position));
        } else if (e.type === "drop") {
          incoming.current = e.paths;
          const spot = spotAt(e.position);
          setTarget(null);
          if (spot && e.paths.length > 0) {
            droppedHere.current = true;
            latest.current.onDrop(e.paths, spot);
          }
        } else {
          setTarget(null);
        }
      })
      .then((stop) => {
        if (alive) unlisten = stop;
        else stop();
      });
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  const press = useCallback((ev: React.MouseEvent, _entry: Entry, dragPaths: () => DragPaths, onStart?: () => void) => {
    // The left button only; Ctrl+click opens the menu on macOS.
    if (ev.button !== 0 || (isMac && ev.ctrlKey)) return;
    const x0 = ev.clientX;
    const y0 = ev.clientY;
    const item = ev.currentTarget as HTMLElement;
    const stop = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", stop);
    };
    const move = (m: MouseEvent) => {
      if (Math.hypot(m.clientX - x0, m.clientY - y0) < DRAG_THRESHOLD) return;
      stop();
      // Read when the drag starts: the press may have changed the selection.
      const paths = dragPaths();
      if (paths && !Array.isArray(paths)) return refuse(item, paths.refused);
      if (!paths || paths.length === 0) return;
      onStart?.();
      droppedHere.current = false;
      api.startDrag(paths).then(
        (dropped) => {
          if (dropped && !droppedHere.current) latest.current.onDraggedAway(paths);
        },
        (e) => latest.current.onError(errorText(e)),
      );
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", stop);
  }, []);

  /** A drag that can't start: the not-allowed cursor until the button is let go, a shake of the
   * item, and the reason. */
  const refuse = (item: HTMLElement, reason: string) => {
    const root = document.documentElement;
    root.classList.add("drag-refused");
    const release = () => {
      root.classList.remove("drag-refused");
      window.removeEventListener("mouseup", release);
      window.removeEventListener("blur", release);
    };
    window.addEventListener("mouseup", release);
    window.addEventListener("blur", release);
    // An animation, not a class: the element's classes belong to React.
    item.animate(
      [0, -5, 5, -4, 4, -2, 0].map((x) => ({ transform: `translateX(${x}px)` })),
      { duration: 360, easing: "ease-out" },
    );
    latest.current.onRefused(reason);
  };

  return { target, press };
}
