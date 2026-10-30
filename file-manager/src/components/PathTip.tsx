import { Fragment, useCallback, useEffect, useLayoutEffect, useRef, useState, type MouseEvent } from "react";
import { createPortal } from "react-dom";
import type { Entry } from "../lib/api";
import { iconFor } from "../lib/kinds";

/** Hover time before a tip shows, as a native tooltip waits. */
const SHOW_DELAY_MS = 450;
/** Moving to the next item within this long after a tip hid shows its tip at once. */
const WARM_MS = 350;
const GAP = 6;
const MARGIN = 12;
/** Folder names longer than this may break mid-name; shorter ones only after a slash. */
const LONG_SEGMENT = 32;

export type TipAlign = "center" | "start";

interface Tip {
  entry: Entry;
  anchor: DOMRect;
  align: TipAlign;
}

/**
 * Hover tips with an entry's full path: search results come from many
 * folders, and the same name often repeats. `bind` gives an item its mouse
 * handlers; `tip` is the popover to render (null while none shows). A click,
 * a key, a scroll or new `entries` hide it, since the item may have moved.
 */
export function usePathTips(enabled: boolean, entries: Entry[]) {
  const [tip, setTip] = useState<Tip | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const warmUntil = useRef(0);

  const hide = useCallback(() => {
    window.clearTimeout(timer.current);
    setTip(null);
  }, []);

  useEffect(() => {
    if (!enabled) {
      hide();
      return;
    }
    window.addEventListener("mousedown", hide, true);
    window.addEventListener("keydown", hide, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("blur", hide);
    return () => {
      hide();
      window.removeEventListener("mousedown", hide, true);
      window.removeEventListener("keydown", hide, true);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("blur", hide);
    };
  }, [enabled, hide]);

  // A new batch of results re-sorts the items; a re-sort with the same count
  // needs a click on a column header, which hides the tip already.
  useEffect(hide, [entries.length, hide]);

  const bind = (entry: Entry, align: TipAlign = "center") =>
    enabled
      ? {
          onMouseEnter: (ev: MouseEvent<HTMLElement>) => {
            const el = ev.currentTarget;
            window.clearTimeout(timer.current);
            const delay = Date.now() < warmUntil.current ? 0 : SHOW_DELAY_MS;
            timer.current = window.setTimeout(() => setTip({ entry, anchor: el.getBoundingClientRect(), align }), delay);
          },
          onMouseLeave: () => {
            if (tip) warmUntil.current = Date.now() + WARM_MS;
            hide();
          },
        }
      : {};

  return { bind, tip };
}

/** The popover: the entry's name, then its full path with the searched folder dimmed. */
export function PathTip({ tip, root }: { tip: Tip; root: string }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  // Below the item, or above it when the window ends first; always inside the window.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { anchor, align } = tip;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const left = align === "center" ? anchor.left + anchor.width / 2 - w / 2 : anchor.left;
    let top = anchor.bottom + GAP;
    if (top + h > window.innerHeight - MARGIN) top = Math.max(MARGIN, anchor.top - GAP - h);
    setPos({ left: Math.min(Math.max(MARGIN, left), window.innerWidth - MARGIN - w), top });
  }, [tip]);

  const { entry } = tip;
  const { Icon, color } = iconFor(entry);
  const prefix = root.endsWith("/") ? root : `${root}/`;
  // The searched folder (dim), the folders inside it, then the name itself.
  const dimmed = entry.path.startsWith(prefix) ? prefix.split("/").length - 1 : 0;
  const parts = entry.path.split("/");
  const toneOf = (i: number) => (i === parts.length - 1 ? "text-ink" : i < dimmed ? "text-dim" : "text-muted");

  return createPortal(
    <div
      ref={ref}
      role="tooltip"
      className="float-panel pointer-events-none fixed z-50 max-w-[26rem] px-3.5 py-2.5"
      style={{ left: pos?.left ?? 0, top: pos?.top ?? 0, visibility: pos ? "visible" : "hidden" }}
    >
      <div className="flex min-w-0 items-center gap-2">
        <Icon className="h-3.5 w-3.5 flex-none" style={{ color }} strokeWidth={1.9} />
        <span className="truncate text-[0.76rem] font-semibold text-ink">{entry.name}</span>
      </div>
      {/* Lines break after a slash, not at a hyphen inside a name. */}
      <p className="mt-1 text-[0.7rem] leading-[1.05rem]">
        {parts.map((part, i) => (
          <Fragment key={i}>
            <span className={`${toneOf(i)} ${part.length > LONG_SEGMENT ? "[overflow-wrap:anywhere]" : "whitespace-nowrap"}`}>
              {i < parts.length - 1 ? `${part}/` : part}
            </span>
            <wbr />
          </Fragment>
        ))}
      </p>
    </div>,
    document.body,
  );
}
