import { ArrowBigUp, Check, ChevronRight, ChevronUp, Command, CornerDownLeft, Delete, LoaderCircle, Option, type LucideIcon } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { isMac } from "./Chrome";
import { t } from "../i18n";

/** A shortcut as shown beside a menu item: Cmd/Ctrl, Control (the same on both systems), Option/Alt, Shift and one key. */
export interface Keys {
  mod?: boolean;
  ctrl?: boolean;
  alt?: boolean;
  shift?: boolean;
  key: string;
}

export type MenuItem =
  | {
      label: ReactNode;
      Icon?: LucideIcon;
      /** A picture in the icon's place (an app's own icon, as a data URL). */
      image?: string | null;
      /** A choice of a group (Sort By, Group By): a check mark takes the icon's place. */
      checked?: boolean;
      keys?: Keys;
      disabled?: boolean;
      danger?: boolean;
      /** Finder's alternate items: shown only while Alt (Option) is held, or only while it is not. */
      when?: "alt" | "plain";
      /** Opens a submenu instead of running; "loading" while its items are on their way. */
      submenu?: MenuItem[] | "loading";
      run?: () => void;
    }
  | { heading: string }
  | "separator";

type Action = Exclude<MenuItem, "separator" | { heading: string }>;
const isAction = (item: MenuItem): item is Action => item !== "separator" && !("heading" in item);

const MARGIN = 8;
/** How long the pointer rests on an item before its submenu opens, or moves away before it closes. */
const SUBMENU_DELAY_MS = 150;

/** The items to draw with Alt held or not: alternates picked, and no separator at an end or twice in a row. */
export function visibleItems(items: MenuItem[], alt: boolean): MenuItem[] {
  const shown = items.filter((item) => !isAction(item) || !item.when || item.when === (alt ? "alt" : "plain"));
  return shown.filter((item, i) => {
    if (item !== "separator") return true;
    const before = shown.slice(0, i).filter((x) => x !== "separator");
    const after = shown.slice(i + 1);
    return before.length > 0 && after.length > 0 && after[0] !== "separator";
  });
}

/** macOS draws modifier keys as symbols (lucide icons here); Linux spells them out. */
function Shortcut({ keys }: { keys: Keys }) {
  if (!isMac) {
    const key = keys.key === "Delete" ? "Del" : keys.key;
    return <span className="text-[0.7rem]">{[(keys.mod || keys.ctrl) && "Ctrl", keys.alt && "Alt", keys.shift && "Shift", key].filter(Boolean).join("+")}</span>;
  }
  return (
    <span className="flex items-center gap-0.5 text-[0.72rem]">
      {keys.ctrl && <ChevronUp className="h-3 w-3" strokeWidth={2.4} />}
      {keys.alt && <Option className="h-3 w-3" strokeWidth={2} />}
      {keys.shift && <ArrowBigUp className="h-3 w-3" strokeWidth={2} />}
      {keys.mod && <Command className="h-3 w-3" strokeWidth={2} />}
      {keys.key === "Backspace" ? (
        <Delete className="h-3.5 w-3.5" strokeWidth={2} />
      ) : keys.key === "Enter" ? (
        <CornerDownLeft className="h-3.5 w-3.5" strokeWidth={2} />
      ) : (
        <span>{keys.key}</span>
      )}
    </span>
  );
}

const enabledOf = (items: MenuItem[]) => items.flatMap((item, i) => (isAction(item) && !item.disabled ? [i] : []));
const step = (enabled: number[], cur: number, down: boolean) => {
  const at = enabled.indexOf(cur);
  return (down ? enabled[(at + 1) % enabled.length] : enabled[(at <= 0 ? enabled.length : at) - 1]) ?? cur;
};
/** An item with a submenu (still loading or not). */
const opensSub = (item: MenuItem | undefined): item is Action & { submenu: MenuItem[] | "loading" } =>
  !!item && isAction(item) && item.submenu != null && !item.disabled;
/** A submenu still on its way: one row that says so. */
const loading = (): MenuItem[] => [{ label: t("menu.loadingApps"), disabled: true }];
const subOf = (item: Action & { submenu: MenuItem[] | "loading" }) => (item.submenu === "loading" ? loading() : item.submenu);

/** One panel of the menu: the items, the highlight, and a row per item. */
function Panel({
  items,
  active,
  panelRef,
  style,
  openSub,
  onEnter,
  onLeave,
  onRun,
  onPanelEnter,
}: {
  items: MenuItem[];
  active: number;
  panelRef: RefObject<HTMLDivElement | null>;
  style: CSSProperties;
  /** The item whose submenu is open (drawn highlighted). */
  openSub?: number;
  onEnter: (i: number) => void;
  onLeave: () => void;
  onRun: (i: number) => void;
  onPanelEnter?: () => void;
}) {
  const checks = items.some((item) => isAction(item) && item.checked !== undefined);
  return (
    <div
      ref={panelRef}
      role="menu"
      className="float-panel fixed z-50 min-w-[13.5rem] max-w-[22rem] p-1.5"
      style={style}
      onMouseEnter={onPanelEnter}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((item, i) => {
        if (item === "separator") return <div key={i} className="mx-2 my-1 h-px bg-white/10" />;
        if ("heading" in item)
          return (
            <p key={i} className="px-2.5 pb-1 pt-1.5 text-[0.68rem] font-semibold text-dim">
              {item.heading}
            </p>
          );
        const lit = active === i || openSub === i;
        const loading = item.submenu === "loading";
        return (
          <button
            key={i}
            type="button"
            role="menuitem"
            aria-haspopup={item.submenu ? "menu" : undefined}
            data-item={i}
            disabled={item.disabled}
            onMouseEnter={() => onEnter(item.disabled ? -1 : i)}
            onMouseLeave={onLeave}
            onClick={() => onRun(i)}
            className={`flex h-8 w-full cursor-default items-center gap-2.5 rounded-[7px] px-2.5 text-left text-[0.8rem] disabled:opacity-40 ${
              lit ? "text-paper" : item.danger ? "text-[var(--danger-text)]" : "text-ink"
            }`}
            style={lit ? { background: item.danger ? "rgb(255 92 122 / 0.55)" : "rgb(var(--a1-rgb) / 0.6)" } : undefined}
          >
            {item.checked !== undefined || (checks && !item.Icon && item.image === undefined) ? (
              <Check className={`h-4 w-4 flex-none ${item.checked ? "" : "invisible"}`} strokeWidth={2.2} />
            ) : item.image ? (
              <img src={item.image} alt="" draggable={false} className="h-4 w-4 flex-none" />
            ) : (
              item.Icon && <item.Icon className="h-4 w-4 flex-none" strokeWidth={1.9} />
            )}
            <span className="min-w-0 flex-1 truncate">{item.label}</span>
            {item.keys && (
              <span className={`ml-4 flex-none ${lit ? "text-paper/80" : "text-dim"}`}>
                <Shortcut keys={item.keys} />
              </span>
            )}
            {item.submenu &&
              (loading ? (
                <LoaderCircle className="spin ml-3 h-3.5 w-3.5 flex-none text-dim" />
              ) : (
                <ChevronRight className={`ml-3 h-3.5 w-3.5 flex-none ${lit ? "text-paper/80" : "text-dim"}`} />
              ))}
          </button>
        );
      })}
    </div>
  );
}

/**
 * The context menu: opens at the pointer (flipped to stay inside the window),
 * moves with the arrow keys, runs an item with Enter or a click, and closes
 * on Esc, a click elsewhere, a scroll or the window losing focus. An item
 * with a submenu opens it beside itself on hover, a click, Right or Enter
 * (Left goes back). Alt (Option) held when it opens, or pressed while it is
 * open, shows the alternate items, as in Finder. A toolbar button that opens
 * it is its `anchor`: a press there is the button's own (it closes the menu
 * by toggling it).
 */
export function ContextMenu({
  x,
  y,
  items: all,
  onClose,
  anchor,
  alt: altAtOpen = false,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
  anchor?: HTMLElement | null;
  /** Alt (Option) was held as the menu opened. */
  alt?: boolean;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const subRef = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [alt, setAlt] = useState(altAtOpen);
  const [active, setActive] = useState(-1);
  const [sub, setSub] = useState(-1);
  const [subActive, setSubActive] = useState(-1);
  const [subPos, setSubPos] = useState<{ left: number; top: number } | null>(null);
  // Keys go to the submenu once it was entered (Right, or the pointer).
  const [inSub, setInSub] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  const items = visibleItems(all, alt);
  const subItem = items[sub];
  const subItems = opensSub(subItem) ? subOf(subItem) : null;

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const left = x + w > window.innerWidth - MARGIN ? Math.max(MARGIN, x - w) : x;
    const top = y + h > window.innerHeight - MARGIN ? Math.max(MARGIN, y - h) : y;
    setPos({ left, top });
  }, [x, y]);

  // The submenu beside its item: to the right, or to the left where there is no room.
  useLayoutEffect(() => {
    const row = ref.current?.querySelector<HTMLElement>(`[data-item="${sub}"]`);
    const el = subRef.current;
    if (!row || !el) return setSubPos(null);
    const r = row.getBoundingClientRect();
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const left = r.right + w + 4 > window.innerWidth - MARGIN ? Math.max(MARGIN, r.left - w - 4) : r.right + 4;
    const top = Math.max(MARGIN, Math.min(r.top - 6, window.innerHeight - MARGIN - h));
    setSubPos({ left, top });
  }, [sub, subItems?.length, pos?.left, pos?.top]);

  const closeSub = () => {
    setSub(-1);
    setSubActive(-1);
    setInSub(false);
  };
  const run = (list: MenuItem[], i: number) => {
    const item = list[i];
    if (!item || !isAction(item) || item.disabled || item.submenu) return;
    onClose();
    item.run?.();
  };
  const openSubAt = (i: number, focusFirst: boolean) => {
    window.clearTimeout(timer.current);
    if (!opensSub(items[i])) return;
    setSub(i);
    setSubActive(focusFirst ? (enabledOf(subOf(items[i] as Action & { submenu: MenuItem[] | "loading" }))[0] ?? -1) : -1);
    setInSub(focusFirst);
  };

  useEffect(() => () => window.clearTimeout(timer.current), []);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (!ref.current?.contains(target) && !subRef.current?.contains(target) && !anchor?.contains(target)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Alt") {
        // The alternates change the rows: start the highlight over.
        if (e.type === "keydown" ? !alt : alt) {
          setAlt(e.type === "keydown");
          setActive(-1);
          closeSub();
        }
        return;
      }
      if (e.type !== "keydown") return;
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        const down = e.key === "ArrowDown";
        // From the latest highlight: keys can arrive faster than a render.
        if (inSub && subItems) setSubActive((cur) => step(enabledOf(subItems), cur, down));
        else setActive((cur) => step(enabledOf(items), cur, down));
      } else if (e.key === "ArrowRight") {
        if (!inSub && active >= 0) openSubAt(active, true);
      } else if (e.key === "ArrowLeft") {
        if (inSub) closeSub();
      } else if (e.key === "Enter") {
        if (inSub && subItems) run(subItems, subActive);
        else if (opensSub(items[active])) openSubAt(active, true);
        else run(items, active);
      } else return;
      // The menu owns these keys while open (the window would move the selection).
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey, true);
    window.addEventListener("keyup", onKey, true);
    window.addEventListener("scroll", onClose, true);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("keyup", onKey, true);
      window.removeEventListener("scroll", onClose, true);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  });

  return createPortal(
    <>
      <Panel
        items={items}
        active={active}
        openSub={sub}
        panelRef={ref}
        style={{ left: pos?.left ?? x, top: pos?.top ?? y, visibility: pos ? "visible" : "hidden" }}
        onEnter={(i) => {
          setActive(i);
          setInSub(false);
          window.clearTimeout(timer.current);
          // A short rest opens a submenu, or closes the open one; the pointer
          // can cross another row on its way into the submenu.
          timer.current = window.setTimeout(() => {
            if (opensSub(items[i])) openSubAt(i, false);
            else closeSub();
          }, SUBMENU_DELAY_MS);
        }}
        onLeave={() => setActive(-1)}
        onRun={(i) => (opensSub(items[i]) ? openSubAt(i, false) : run(items, i))}
      />
      {subItems && (
        <Panel
          items={subItems}
          active={subActive}
          panelRef={subRef}
          style={{ left: subPos?.left ?? 0, top: subPos?.top ?? 0, visibility: subPos ? "visible" : "hidden" }}
          onPanelEnter={() => {
            window.clearTimeout(timer.current);
            setInSub(true);
          }}
          onEnter={setSubActive}
          onLeave={() => setSubActive(-1)}
          onRun={(i) => run(subItems, i)}
        />
      )}
    </>,
    document.body,
  );
}
