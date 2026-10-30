import {
  Download,
  FileText,
  Film,
  Folder,
  FolderGit2,
  FolderX,
  HardDrive,
  House,
  Image,
  Laptop,
  Monitor,
  Music,
  PanelLeftClose,
  PanelLeftOpen,
  Trash2,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import type { Place, PlaceIcon, Volume } from "../lib/api";
import { dropAttr, spotKey } from "../lib/dnd";
import { bytesText } from "../ui/format";
import { repoText } from "../lib/repoUsage";
import { t } from "../i18n";
import { isMac } from "./Chrome";

export const PLACE_ICON: Record<PlaceIcon, LucideIcon> = {
  drive: FolderGit2,
  home: House,
  desktop: Monitor,
  documents: FileText,
  downloads: Download,
  pictures: Image,
  movies: Film,
  music: Music,
  folder: Folder,
  disk: HardDrive,
  trash: Trash2,
};

export const VOLUME_ICON: Record<Volume["kind"], LucideIcon> = {
  system: HardDrive,
  drive: FolderGit2,
  external: HardDrive,
};

/** Hide or show the sidebar: Ctrl+Cmd+S on macOS (Finder), F9 on Linux (Files). */
export const SIDEBAR_KEYS = isMac ? "Ctrl+Cmd+S" : "F9";

/** How far the pointer travels before a press on a bookmark starts moving it. */
const REORDER_START = 4;
/** How long the pointer rests on an icon before its name shows; moving on to the next icon shows it at once. */
const HOVER_DELAY_MS = 200;
const HOVER_CHAIN_MS = 400;

/** The name beside a collapsed sidebar's icon. */
interface Hover {
  label: string;
  detail?: string;
  x: number;
  y: number;
}

/**
 * The collapsed sidebar names its icons in a popover beside them, as a
 * tooltip would, but at once and in the app's look. `bind` gives a row its
 * pointer handlers; a press (a bookmark being moved, a click) hides it.
 */
function useHoverLabel(enabled: boolean) {
  const [hover, setHover] = useState<Hover | null>(null);
  const timer = useRef<number | undefined>(undefined);
  // Whether a name shows, and when the last one went (moving on from it shows the next at once).
  const showing = useRef(false);
  const hiddenAt = useRef(0);
  const show = (h: Hover) => {
    showing.current = true;
    setHover(h);
  };
  const hide = () => {
    window.clearTimeout(timer.current);
    if (showing.current) hiddenAt.current = Date.now();
    showing.current = false;
    setHover(null);
  };
  useEffect(() => {
    if (!enabled) hide();
    window.addEventListener("blur", hide);
    window.addEventListener("scroll", hide, true);
    return () => {
      window.clearTimeout(timer.current);
      window.removeEventListener("blur", hide);
      window.removeEventListener("scroll", hide, true);
    };
  }, [enabled]);
  const bind = (label: string, detail?: string) =>
    enabled
      ? {
          onMouseEnter: (e: React.MouseEvent<HTMLElement>) => {
            const b = e.currentTarget.getBoundingClientRect();
            const next = { label, detail, x: b.right + 10, y: b.top + b.height / 2 };
            window.clearTimeout(timer.current);
            if (Date.now() - hiddenAt.current < HOVER_CHAIN_MS) show(next);
            else timer.current = window.setTimeout(() => show(next), HOVER_DELAY_MS);
          },
          onMouseLeave: hide,
          onMouseDownCapture: hide,
        }
      : {};
  return { hover, bind };
}

function HoverLabel({ hover }: { hover: Hover | null }) {
  if (!hover) return null;
  return createPortal(
    <div
      role="tooltip"
      className="float-panel fade-in pointer-events-none fixed z-50 -translate-y-1/2 whitespace-nowrap px-3 py-1.5"
      style={{ left: hover.x, top: hover.y, borderRadius: 10 }}
    >
      <p className="text-[0.78rem] font-medium text-ink">{hover.label}</p>
      {hover.detail && <p className="text-[0.68rem] text-dim">{hover.detail}</p>}
    </div>,
    document.body,
  );
}

function Row({
  label,
  title,
  Icon,
  on,
  dim = false,
  collapsed,
  highlight = false,
  onClick,
  onMouseDown,
  onContextMenu,
  attrs,
  hoverProps,
  children,
}: {
  label: string;
  title?: string;
  Icon: LucideIcon;
  on: boolean;
  /** A bookmark whose folder is gone. */
  dim?: boolean;
  collapsed: boolean;
  /** A drag is over it. */
  highlight?: boolean;
  onClick: () => void;
  onMouseDown?: (ev: React.MouseEvent) => void;
  onContextMenu?: (ev: React.MouseEvent) => void;
  attrs?: Record<string, string | undefined>;
  /** The collapsed sidebar's name popover (`useHoverLabel`). */
  hoverProps?: React.HTMLAttributes<HTMLElement>;
  /** Drawn over the row (the insertion line of a bookmark being moved). */
  children?: ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      onMouseDown={onMouseDown}
      onContextMenu={onContextMenu}
      title={collapsed ? undefined : title}
      aria-label={label}
      {...attrs}
      {...hoverProps}
      className={`relative flex h-9 flex-none items-center rounded-xl text-[0.82rem] font-medium transition-colors ${
        collapsed ? "mx-auto w-10 justify-center" : "gap-3 px-3"
      } ${highlight ? "drop-target" : on ? "bg-white/10 text-ink" : "text-muted hover:bg-white/5 hover:text-ink"} ${dim ? "opacity-45" : ""}`}
    >
      {on && !collapsed && (
        <span className="absolute left-0 top-1/2 h-4 w-1 -translate-y-1/2 rounded-full" style={{ background: "var(--accent)" }} />
      )}
      <Icon className={`h-4 w-4 flex-none ${on ? "accent-fg" : ""}`} strokeWidth={2} />
      {!collapsed && <span className="truncate">{label}</span>}
      {children}
    </button>
  );
}

/** Where a bookmark lands: a line in the accent between two rows. */
function InsertLine({ at }: { at: "top" | "bottom" }) {
  return (
    <span
      className={`pointer-events-none absolute inset-x-2 h-0.5 rounded-full ${at === "top" ? "-top-[2px]" : "-bottom-[2px]"}`}
      style={{ background: "var(--accent)" }}
    />
  );
}

/** A section's title with an optional button at its right; collapsed, the button alone (centered), or a divider. */
function Heading({ children, collapsed, action }: { children: string; collapsed: boolean; action?: ReactNode }) {
  if (collapsed) return action ? <div className="mb-1.5 flex h-7 flex-none justify-center">{action}</div> : <div className="mx-auto my-2 h-px w-8 flex-none bg-white/10" />;
  return (
    <div className="mb-1.5 flex h-7 flex-none items-center justify-between pl-3 pr-0.5">
      <p className="label">{children}</p>
      {action}
    </div>
  );
}

/**
 * The sidebar: the user's bookmarks (moved by dragging, added by a drop
 * between them or the menu of a folder; removed from their own menu), then
 * the locations: Computer, the mounted volumes and the Trash. The button
 * beside "Bookmarks" collapses it into a strip of icons, each named on
 * hover, with that button on top.
 */
export function Sidebar({
  collapsed,
  bookmarks,
  volumes,
  trash,
  active,
  onOpen,
  onReorder,
  onBookmarkMenu,
  target,
  onToggle,
  footer,
}: {
  collapsed: boolean;
  bookmarks: Place[];
  volumes: Volume[];
  /** The folder the Trash shows (null until known). */
  trash: string | null;
  /** The item to highlight (`activeItem`). */
  active: string | null;
  /** Opens a folder, or the Computer view (`COMPUTER`). */
  onOpen: (path: string) => void;
  onReorder: (paths: string[]) => void;
  onBookmarkMenu: (ev: React.MouseEvent, place: Place) => void;
  /** The drop spot a drag is over (`spotKey`). */
  target: string | null;
  /** Collapses or expands the sidebar. */
  onToggle: () => void;
  /** At the foot, against the bottom edge (the file operations under way). */
  footer?: ReactNode;
}) {
  const list = useRef<HTMLDivElement | null>(null);
  const [moving, setMoving] = useState<{ from: number; to: number } | null>(null);
  // The click that ends a move is not a click on the bookmark.
  const swallowClick = useRef(false);
  const names = useHoverLabel(collapsed && moving == null && target == null);

  /** A press on a bookmark: past a few pixels it moves the bookmark, dropped between two others. */
  const press = (ev: React.MouseEvent, from: number) => {
    if (ev.button !== 0 || (isMac && ev.ctrlKey)) return;
    const y0 = ev.clientY;
    let to: number | null = null;
    const slot = (y: number) =>
      [...(list.current?.querySelectorAll<HTMLElement>("[data-bookmark-index]") ?? [])].filter((row) => {
        const b = row.getBoundingClientRect();
        return b.top + b.height / 2 < y;
      }).length;
    const move = (m: MouseEvent) => {
      if (to == null && Math.abs(m.clientY - y0) < REORDER_START) return;
      to = slot(m.clientY);
      setMoving({ from, to });
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      if (to == null) return;
      swallowClick.current = true;
      window.setTimeout(() => (swallowClick.current = false), 0);
      setMoving(null);
      if (to === from || to === from + 1) return;
      const paths = bookmarks.map((b) => b.path);
      const [moved] = paths.splice(from, 1);
      paths.splice(to > from ? to - 1 : to, 0, moved);
      onReorder(paths);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  // The gap a bookmark (moved here, or dropped from a view) would land in.
  const gap = moving ? moving.to : target?.startsWith("bookmark:") ? Number(target.slice("bookmark:".length)) : null;

  return (
    <aside
      data-tauri-drag-region
      className={`relative z-10 flex flex-none flex-col overflow-hidden pb-4 transition-[width] duration-200 ${
        collapsed ? "w-[4.25rem] px-1.5" : "w-56 px-3.5"
      } ${isMac ? "pt-14" : "pt-6"}`}
    >
      <div ref={list} data-bookmarks className="flex flex-none flex-col pb-3">
        <Heading
          collapsed={collapsed}
          action={
            <button
              aria-label={collapsed ? t("sidebar.show") : t("sidebar.hide")}
              title={`${collapsed ? t("sidebar.show") : t("sidebar.hide")} (${SIDEBAR_KEYS})`}
              onClick={onToggle}
              className="grid h-7 w-7 place-items-center rounded-lg text-dim transition-colors hover:bg-white/8 hover:text-ink"
            >
              {collapsed ? <PanelLeftOpen className="h-4 w-4" /> : <PanelLeftClose className="h-4 w-4" />}
            </button>
          }
        >
          {t("sidebar.bookmarks")}
        </Heading>
        <nav className="flex flex-col gap-0.5">
          {bookmarks.map((p, i) => (
            <Row
              key={p.path}
              label={p.label}
              title={p.missing ? t("sidebar.notFoundPath", { path: p.path }) : p.path}
              Icon={p.missing ? FolderX : PLACE_ICON[p.icon]}
              on={active === `b:${p.path}`}
              dim={p.missing || moving?.from === i}
              collapsed={collapsed}
              highlight={!p.missing && target === spotKey("place", p.path)}
              onClick={() => {
                if (!swallowClick.current) onOpen(p.path);
              }}
              onMouseDown={(ev) => press(ev, i)}
              onContextMenu={(ev) => {
                ev.preventDefault();
                onBookmarkMenu(ev, p);
              }}
              attrs={{ "data-bookmark-index": String(i), ...(p.missing ? {} : dropAttr(p.path, "place")) }}
              hoverProps={names.bind(p.label, p.missing ? t("sidebar.folderNotFound") : undefined)}
            >
              {gap === i && <InsertLine at="top" />}
              {gap === bookmarks.length && i === bookmarks.length - 1 && <InsertLine at="bottom" />}
            </Row>
          ))}
          {bookmarks.length === 0 && !collapsed && (
            <p className={`rounded-xl px-3 py-2 text-[0.72rem] text-dim ${gap === 0 ? "drop-target" : ""}`}>{t("sidebar.dropHere")}</p>
          )}
        </nav>
      </div>

      <Heading collapsed={collapsed}>{t("sidebar.locations")}</Heading>
      <nav className="flex min-h-0 flex-col gap-0.5 overflow-y-auto">
        <Row
          label={t("sidebar.computer")}
          Icon={Laptop}
          on={active === "computer"}
          collapsed={collapsed}
          onClick={() => onOpen("computer:")}
          hoverProps={names.bind(t("sidebar.computer"))}
        />
        {volumes.map((v) => (
          <Row
            key={v.path}
            label={v.name}
            title={v.repo ? `${v.path}\n${repoText(v.repo)}` : v.total > 0 ? `${v.path}\n${t("sidebar.freeOf", { free: bytesText(v.free), total: bytesText(v.total) })}` : v.path}
            Icon={VOLUME_ICON[v.kind]}
            on={active === `v:${v.path}`}
            collapsed={collapsed}
            highlight={target === spotKey("place", v.path)}
            onClick={() => onOpen(v.path)}
            attrs={dropAttr(v.path, "place")}
            hoverProps={names.bind(
              v.name,
              v.repo ? repoText(v.repo) : v.total > 0 ? t("sidebar.freeOf", { free: bytesText(v.free), total: bytesText(v.total) }) : undefined,
            )}
          />
        ))}
        {trash && (
          <Row
            label={t("sidebar.trash")}
            Icon={Trash2}
            on={active === "trash"}
            collapsed={collapsed}
            highlight={target === spotKey("trash", trash)}
            onClick={() => onOpen(trash)}
            attrs={dropAttr(trash, "trash")}
            hoverProps={names.bind(t("sidebar.trash"))}
          />
        )}
      </nav>
      {footer && <div className="mt-auto flex-none pt-3">{footer}</div>}
      <HoverLabel hover={names.hover} />
    </aside>
  );
}
