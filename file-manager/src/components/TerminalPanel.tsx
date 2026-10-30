import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal, type ILink, type ITheme } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ChevronDown, Columns2, FolderInput, Maximize2, Minimize2, Plus, Rows2, Server, SquareTerminal, X } from "lucide-react";
import { forwardRef, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { api, baseName, errorText } from "../lib/api";
import { copyText } from "../lib/clipboard";
import { dropAttr, useIsDropTarget } from "../lib/dnd";
import { isTerminalKey, TERMINAL_KEYS } from "../lib/shortcuts";
import { isLeaf, leaves, remove, setRatio, split, type Layout, type Path } from "../lib/splits";
import { isMac } from "./Chrome";
import { Heading, Modal } from "./Dialog";
// `tr`: the message function (`t` is a tab all over this file).
import { t as tr } from "../i18n";

/** What the window asks of the terminal panel. */
export interface TerminalHandle {
  /** A new tab with the user's shell in `cwd`. */
  newTab: (cwd: string) => void;
  /** Types dropped paths into the pane in front. */
  pastePaths: (paths: string[]) => void;
  focus: () => void;
  tabCount: () => number;
}

/** A tab: its panes as a split tree, and the pane with the keys. */
interface Tab {
  key: number;
  layout: Layout;
  focus: number;
}

/** A pane's live parts, outside React's state. */
interface Live {
  term: Terminal;
  fit: FitAddon;
  /** Holds the terminal for its whole life: it moves between leaves as the tree changes, without restarting anything. */
  wrapper: HTMLDivElement;
  /** Where the shell starts. */
  start: string;
  /** The terminal is open in the wrapper and its shell started. */
  opened: boolean;
  /** The backend's session, once started. */
  id: number | null;
  /** The shell's working folder, as last read (relative links resolve against it). */
  cwd: string;
  /** What the user typed before the shell was there. */
  pending: string[];
  /** The server an `ssh` in front of the shell is connected to; the title the terminal last got (OSC 0/2). */
  ssh: string | null;
  title: string | null;
  /** The command running in front of the shell, as last read. */
  command: string | null;
  /** Watches the pane's box: the terminal refits when it changes. */
  sizes?: ResizeObserver;
  /** The size the shell was last told, and the timer that tells it the next one. */
  sent: { cols: number; rows: number } | null;
  resizeTimer: number;
  /** A full-screen program's background, painted under the whole pane (`null`: the glass shows). */
  backdrop: string | null;
  /** A program has run in front of the shell for a while (seen by two status reads in a row). */
  program: boolean;
  backdropTimer: number;
}

type Direction = "left" | "right" | "up" | "down";

/** The panel's height: its least, its first, and the most of the room it shares with the folders. */
export const TERMINAL_HEIGHT = { min: 120, initial: 260, maxShare: 0.5 };

/** How long a resize must rest before the shell is told (one SIGWINCH per drag, not one per frame). */
const RESIZE_SETTLE_MS = 160;

/**
 * Tells the shell the terminal's size once a resize settles. xterm.js
 * reflows the lines at every step; the shell gets a single SIGWINCH, and
 * when the width changed it redraws its screen if it sits at its prompt
 * (the backend sends Ctrl+L then), so old prompts wrapped by the reflow and
 * copies of a two-line prompt do not pile up.
 */
function tellSize(pane: Live) {
  window.clearTimeout(pane.resizeTimer);
  pane.resizeTimer = window.setTimeout(() => {
    const { cols, rows } = pane.term;
    if (pane.id == null || (pane.sent?.cols === cols && pane.sent?.rows === rows)) return;
    const widthChanged = pane.sent != null && pane.sent.cols !== cols;
    pane.sent = { cols, rows };
    // A pane closed meanwhile has no session left: nothing to tell.
    api.termResize(pane.id, cols, rows, widthChanged).catch(() => {});
  }, RESIZE_SETTLE_MS);
}

/** The 16 colors, in the app's palette (the glass behind shows through the background). */
const ANSI: ITheme = {
  black: "#2a2d45",
  red: "#ff6b85",
  green: "#46e3a4",
  yellow: "#ffb547",
  blue: "#6aa8ff",
  magenta: "#c86bf0",
  cyan: "#3fd0f0",
  white: "#c3cae8",
  brightBlack: "#8186ab",
  brightRed: "#ff8fa3",
  brightGreen: "#7ff0c0",
  brightYellow: "#ffd27a",
  brightBlue: "#9cc4ff",
  brightMagenta: "#dd9cf7",
  brightCyan: "#86e4f7",
  brightWhite: "#eceefb",
};

/** Under a full-screen program whose cells mostly keep the default background (tmux, htop):
 * opaque, so its explicitly colored parts and the rest read as one surface. */
const GRAPHIC_BACKDROP = "#1c1d31";

/** A program's own background counts once it covers this share of the screen (nvim's colorscheme
 * paints every cell; a status bar alone does not). */
const BACKDROP_SHARE = 0.2;

const hexRgb = (hex: string) => parseInt(hex.slice(1, 7), 16);
const ANSI_ORDER = ["black", "red", "green", "yellow", "blue", "magenta", "cyan", "white"] as const;

/** The RGB of palette color `i`: the 16 of the theme, then xterm's 6x6x6 cube and gray ramp. */
function paletteRgb(i: number): number {
  if (i < 16) {
    const name = i < 8 ? ANSI_ORDER[i] : `bright${ANSI_ORDER[i - 8][0].toUpperCase()}${ANSI_ORDER[i - 8].slice(1)}`;
    return hexRgb(ANSI[name as keyof ITheme] as string);
  }
  if (i < 232) {
    const level = (v: number) => (v === 0 ? 0 : 55 + v * 40);
    const n = i - 16;
    return (level(Math.floor(n / 36)) << 16) | (level(Math.floor(n / 6) % 6) << 8) | level(n % 6);
  }
  const g = 8 + (i - 232) * 10;
  return (g << 16) | (g << 8) | g;
}

/**
 * The background a program shows: the color most of its cells carry when it
 * covers enough of the screen, else an opaque dark one. Painted for what draws
 * on the alternate screen (nvim, htop, tmux) and for a program that stays in
 * front on the normal one (Claude Code draws its interface there, an SSH
 * session, a server's log); `null` at the shell's prompt, where the glass
 * shows through. Sampled on a grid of at most ~4,000 cells.
 */
function backdropOf(term: Terminal, program: boolean): string | null {
  const buf = term.buffer.active;
  if (buf.type !== "alternate" && !program) return null;
  const step = Math.max(1, Math.ceil(Math.sqrt((term.rows * term.cols) / 4000)));
  const counts = new Map<number, number>();
  let total = 0;
  const cell = buf.getNullCell();
  for (let y = 0; y < term.rows; y += step) {
    const line = buf.getLine(buf.viewportY + y);
    if (!line) continue;
    for (let x = 0; x < term.cols; x += step) {
      const c = line.getCell(x, cell);
      if (!c || c.isInverse()) continue;
      total++;
      if (c.isBgDefault()) continue;
      const rgb = c.isBgRGB() ? c.getBgColor() : paletteRgb(c.getBgColor());
      counts.set(rgb, (counts.get(rgb) ?? 0) + 1);
    }
  }
  let best = -1;
  let most = 0;
  for (const [rgb, n] of counts) if (n > most) [best, most] = [rgb, n];
  if (best < 0 || most < total * BACKDROP_SHARE) return GRAPHIC_BACKDROP;
  return `#${best.toString(16).padStart(6, "0")}`;
}

/** The pane's theme: the app's, over the backdrop of a full-screen program when one runs. */
const themeOf = (pane: Live): ITheme => (pane.backdrop ? { ...themeNow(), background: pane.backdrop } : themeNow());

/** Looks again, shortly, at what the pane shows: a full-screen program's background is painted
 * under the whole pane (its padding and the part of a cell the grid leaves over included). */
function watchBackdrop(pane: Live) {
  if (pane.backdropTimer) return;
  pane.backdropTimer = window.setTimeout(() => {
    pane.backdropTimer = 0;
    const color = backdropOf(pane.term, pane.program);
    if (color === pane.backdrop) return;
    pane.backdrop = color;
    pane.wrapper.style.background = color ?? "";
    pane.term.options.theme = themeOf(pane);
  }, 150);
}

/** The theme with the accent of the moment: the cursor and the selection follow it. */
function themeNow(): ITheme {
  const css = getComputedStyle(document.documentElement);
  const accent = css.getPropertyValue("--a1").trim() || "#8f6bff";
  const [r, g, b] = (css.getPropertyValue("--a1-rgb").trim() || "143 107 255").split(/\s+/);
  // Dark on both themes (the panel stays dark on the light one, `term-dark`).
  return {
    ...ANSI,
    background: "rgba(0, 0, 0, 0)",
    foreground: "#eceefb",
    cursor: accent,
    cursorAccent: "#16162b",
    selectionBackground: `rgba(${r}, ${g}, ${b}, 0.38)`,
  };
}

const modHeld = (e: MouseEvent) => (isMac ? e.metaKey : e.ctrlKey);

/** Nerd Font builds first (the icons of prompt themes), then the app's JetBrains Mono and the system's. */
const FONT_FAMILY = [
  "JetBrainsMono Nerd Font Mono",
  "JetBrainsMono Nerd Font",
  "JetBrains Mono",
  "MesloLGS NF",
  "MesloLGL Nerd Font Mono",
  "Hack Nerd Font Mono",
  "FiraCode Nerd Font Mono",
  "Symbols Nerd Font Mono",
  "Menlo",
  "DejaVu Sans Mono",
  "monospace",
]
  .map((f) => (f === "monospace" ? f : `"${f}"`))
  .join(", ");

/** Words of a terminal line that may be paths, with where each starts (0-based). */
function pathCandidates(line: string): { text: string; start: number }[] {
  const out: { text: string; start: number }[] = [];
  for (const m of line.matchAll(/[^\s'"`<>|;()[\]{}]+/g)) {
    // `file.rs:12:5` and a trailing comma or period still point at the file.
    const text = m[0].replace(/(:\d+)+:?$/, "").replace(/[.,:]+$/, "");
    if (text.length > 0 && text.length < 1024 && !/^https?:/.test(text)) out.push({ text, start: m.index ?? 0 });
  }
  return out.slice(0, 60);
}

function resolvePath(text: string, cwd: string, home: string): string {
  if (text.startsWith("/")) return text;
  if (text === "~") return home;
  if (text.startsWith("~/")) return `${home}${text.slice(1)}`;
  return `${cwd.replace(/\/$/, "")}/${text}`;
}

/**
 * The terminal panel, as in VS Code: tabs of the user's login shell under
 * the folders, in the app's glass. A tab splits into panes side by side
 * (Cmd+D) or one above the other (Cmd+Shift+D), as in iTerm, with dividers
 * that drag; Cmd+Option+arrows move between panes and Cmd+W closes one.
 * Each pane is an xterm.js terminal on a pseudo-terminal of the backend;
 * hiding the panel keeps the shells. Cmd+click (Ctrl+click) on a web link
 * opens the browser; on a path, the app goes there. Paths dropped on it are
 * typed into the pane in front, quoted.
 */
export const TerminalPanel = forwardRef<
  TerminalHandle,
  {
    visible: boolean;
    height: number;
    onHeight: (h: number) => void;
    /** Where a new tab starts, and where the "go to the folder in view" button goes (null: no folder in view). */
    folder: string | null;
    home: string;
    /** The accent id: the theme is read again when it changes. */
    accent: string;
    /** The theme shown: the terminal's colors follow it. */
    theme: "light" | "dark";
    onHide: () => void;
    /** Over the folders: the panel takes their room (the sidebar and the toolbar stay). */
    maximized: boolean;
    onMaximize: () => void;
    /** A path clicked in a pane: a folder opens, a file is selected in its folder. */
    onPath: (path: string, isDir: boolean) => void;
    onError: (text: string) => void;
  }
>(function TerminalPanel({ visible, height, onHeight, folder, home, accent, theme, onHide, maximized, onMaximize, onPath, onError }, ref) {
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [active, setActive] = useState<number | null>(null);
  // The shells' folders by pane: tab titles.
  const [cwds, setCwds] = useState<Record<number, string>>({});
  const live = useRef(new Map<number, Live>());
  const nextKey = useRef(1);
  // Tabs counted as they open and close, not at the next render: two calls
  // in a row (React's development mode runs effects twice) see each other.
  const count = useRef(0);
  const body = useRef<HTMLDivElement | null>(null);
  const tabsRef = useRef(tabs);
  tabsRef.current = tabs;
  const activeRef = useRef(active);
  activeRef.current = active;
  const latest = useRef({ onPath, onError, onHide, home });
  latest.current = { onPath, onError, onHide, home };
  const dropHere = useIsDropTarget("terminal", "terminal");

  const activeTab = tabs.find((t) => t.key === active);
  const focusedLive = activeTab ? live.current.get(activeTab.focus) : undefined;

  /** Fits every pane of the tab in front to its leaf. */
  const fitAll = useCallback(() => {
    const tab = tabsRef.current.find((t) => t.key === activeRef.current);
    if (!tab || !visible) return;
    for (const key of leaves(tab.layout)) {
      const p = live.current.get(key);
      if (!p?.opened) continue;
      try {
        p.fit.fit();
      } catch {
        // Not laid out yet: the next resize fits it.
      }
    }
  }, [visible]);

  const focusPane = useCallback((tabKey: number, key: number) => {
    setTabs((list) => list.map((t) => (t.key === tabKey && t.focus !== key ? { ...t, focus: key } : t)));
    live.current.get(key)?.term.focus();
  }, []);

  /** Ends pane `key` (closed, or its shell exited): its sibling takes its place; the last one closes its tab. */
  const removePane = useCallback((key: number) => {
    const p = live.current.get(key);
    if (p) {
      if (p.id != null) void api.termClose(p.id);
      p.sizes?.disconnect();
      window.clearTimeout(p.resizeTimer);
      window.clearTimeout(p.backdropTimer);
      p.term.dispose();
      p.wrapper.remove();
      live.current.delete(key);
    }
    setTabs((list) => {
      const next: Tab[] = [];
      for (const t of list) {
        if (!leaves(t.layout).includes(key)) {
          next.push(t);
          continue;
        }
        const layout = remove(t.layout, key);
        if (layout == null) {
          count.current = Math.max(0, count.current - 1);
          continue;
        }
        const focus = t.focus === key ? leaves(layout)[0] : t.focus;
        next.push({ ...t, layout, focus });
        requestAnimationFrame(() => live.current.get(focus)?.term.focus());
      }
      setActive((cur) => (next.some((t) => t.key === cur) ? cur : (next[next.length - 1]?.key ?? null)));
      // The last shell gone: the panel goes too, as in VS Code.
      if (next.length === 0) latest.current.onHide();
      return next;
    });
  }, []);

  /** A pane's terminal and its shell, made once; they live in `wrapper` from then on. */
  const makePane = useCallback((key: number, start: string): Live => {
    const wrapper = document.createElement("div");
    wrapper.className = "term-wrapper";
    const term = new Terminal({
      // The app's JetBrains Mono, from the Nerd Font build when it is installed: prompt
      // themes (Powerlevel10k, Starship) draw their icons with its symbols.
      fontFamily: FONT_FAMILY,
      fontSize: 12.5,
      lineHeight: 1.25,
      cursorBlink: true,
      cursorStyle: "bar",
      allowTransparency: true,
      macOptionIsMeta: false,
      scrollback: 5000,
      // A clear screen (the redraw after a resize, Ctrl+L, `clear`) keeps what was on it in the scrollback.
      scrollOnEraseInDisplay: true,
      // Glyphs wider than a cell (some icon fonts) are scaled into it.
      rescaleOverlappingGlyphs: true,
      // Unicode 11 widths (below), as tmux and today's shells count them.
      allowProposedApi: true,
      theme: themeNow(),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon((e, uri) => modHeld(e) && void api.openUrl(uri).catch((err) => latest.current.onError(errorText(err)))));
    const pane: Live = { term, fit, wrapper, start, opened: false, id: null, cwd: start, pending: [], ssh: null, title: null, command: null, sent: null, resizeTimer: 0, backdrop: null, backdropTimer: 0, program: false };
    // Its own size: a divider dragged, a split, the panel or the window resized (hidden: nothing to fit).
    // Fitted in the observer's callback itself (after layout, before paint): the box's size never
    // depends on the terminal inside it, so this cannot loop. Kept on the pane: an observer nothing
    // refers to is collected, and stops.
    pane.sizes = new ResizeObserver(() => {
      if (!pane.opened || wrapper.clientWidth === 0 || wrapper.clientHeight === 0) return;
      try {
        fit.fit();
      } catch {
        // Not laid out yet: the next resize fits it.
      }
    });
    pane.sizes.observe(wrapper);
    live.current.set(key, pane);
    return pane;
  }, []);

  /** Opens pane `key`'s terminal (now that it has a place on screen) and starts its shell. */
  const openPane = useCallback(
    (key: number, pane: Live) => {
      const { term, fit } = pane;
      term.open(pane.wrapper);
      pane.opened = true;
      // The WebGL renderer draws box drawing and powerline symbols itself, filling
      // the whole cell at any line height: the DOM one leaves gaps above and
      // below them (a tmux or prompt bar broke into brackets). It falls back to
      // the DOM one when WebGL is missing or its context is lost.
      try {
        const webgl = new WebglAddon();
        webgl.onContextLoss(() => webgl.dispose());
        term.loadAddon(webgl);
      } catch {
        // The DOM renderer stays.
      }
      term.loadAddon(new Unicode11Addon());
      term.unicode.activeVersion = "11";
      // A title set by what runs in it (a remote shell, tmux): shown over SSH.
      term.onTitleChange((t) => {
        pane.title = t || null;
        setCwds((c) => ({ ...c }));
      });
      try {
        fit.fit();
      } catch {
        // Fitted on the first resize.
      }
      // Window and panel shortcuts stay theirs: the shell never sees them.
      term.attachCustomKeyEventHandler((e) => {
        const down = e.type === "keydown";
        const k = e.key.toLowerCase();
        if (isTerminalKey(e)) return false;
        const shortcut = panelShortcut(e);
        if (shortcut) {
          if (down) runShortcut.current(shortcut);
          return false;
        }
        if (isMac && e.metaKey && k === "k") {
          if (down) term.clear();
          return false;
        }
        // Linux: Ctrl+C stays the shell's interrupt; copy and paste take Shift, as in GNOME's Terminal.
        if (!isMac && e.ctrlKey && e.shiftKey && (e.code === "KeyC" || e.code === "KeyV")) {
          if (down && e.code === "KeyC" && term.hasSelection()) void copyText(term.getSelection());
          if (down && e.code === "KeyV")
            navigator.clipboard.readText().then(
              (text) => term.paste(text),
              () => latest.current.onError(tr("terminal.clipboardUnreadable")),
            );
          return false;
        }
        return true;
      });
      const send = (d: string) => {
        if (pane.id == null) pane.pending.push(d);
        else void api.termWrite(pane.id, d);
      };
      term.onData(send);
      term.onBinary(send);
      term.onResize(() => {
        tellSize(pane);
        watchBackdrop(pane);
      });
      // A full-screen program (nvim, htop, tmux) draws on the alternate screen. There a clear
      // erases in place: scrolled away, as on the normal screen, lines with only its background
      // stayed behind and parts of the screen lost its color after a resize.
      term.buffer.onBufferChange((b) => {
        term.options.scrollOnEraseInDisplay = b.type === "normal";
        watchBackdrop(pane);
      });
      term.onWriteParsed(() => {
        if (term.buffer.active.type === "alternate" || pane.backdrop || pane.program) watchBackdrop(pane);
      });
      term.textarea?.addEventListener("focus", () => {
        const tab = tabsRef.current.find((t) => leaves(t.layout).includes(key));
        if (tab && tab.focus !== key) setTabs((list) => list.map((t) => (t.key === tab.key ? { ...t, focus: key } : t)));
      });
      // Paths in the output: underlined on hover, followed with Cmd (Ctrl) held.
      term.registerLinkProvider({
        provideLinks(y, callback) {
          const line = term.buffer.active.getLine(y - 1)?.translateToString(true) ?? "";
          const words = pathCandidates(line);
          if (words.length === 0) return callback(undefined);
          const paths = words.map((w) => resolvePath(w.text, pane.cwd, latest.current.home));
          api.pathKinds(paths).then(
            (kinds) => {
              const links: ILink[] = [];
              kinds.forEach((isDir, i) => {
                if (isDir == null) return;
                const w = words[i];
                links.push({
                  range: { start: { x: w.start + 1, y }, end: { x: w.start + w.text.length, y } },
                  text: w.text,
                  decorations: { underline: true, pointerCursor: true },
                  activate: (e) => modHeld(e) && latest.current.onPath(paths[i], isDir),
                });
              });
              callback(links.length > 0 ? links : undefined);
            },
            () => callback(undefined),
          );
        },
      });
      api
        .termOpen(
          pane.start,
          term.cols,
          term.rows,
          (bytes) => term.write(bytes),
          () => removePane(key),
        )
        .then(
          (id) => {
            pane.id = id;
            for (const d of pane.pending.splice(0)) void api.termWrite(id, d);
            // The size may have changed while the shell started.
            pane.sent = { cols: term.cols, rows: term.rows };
            void api.termResize(id, term.cols, term.rows);
          },
          (e) => {
            latest.current.onError(errorText(e));
            removePane(key);
          },
        );
      term.focus();
    },
    [removePane],
  );

  /** A leaf of the tree on screen: the pane's wrapper moves into it, opened the first time. */
  const mount = useCallback(
    (key: number, el: HTMLDivElement | null) => {
      const pane = el && live.current.get(key);
      if (!el || !pane) return;
      if (pane.wrapper.parentElement !== el) el.appendChild(pane.wrapper);
      if (!pane.opened) openPane(key, pane);
      requestAnimationFrame(fitAll);
    },
    [openPane, fitAll],
  );

  const newTab = useCallback(
    (cwd: string) => {
      const key = nextKey.current++;
      count.current += 1;
      makePane(key, cwd);
      setTabs((list) => [...list, { key, layout: { leaf: key }, focus: key }]);
      setActive(key);
    },
    [makePane],
  );

  /** Splits the pane in front: a new shell in its folder, beside it ("row") or below it ("column"). */
  const splitFocused = useCallback(
    (dir: "row" | "column") => {
      const tab = tabsRef.current.find((t) => t.key === activeRef.current);
      if (!tab) return;
      const from = live.current.get(tab.focus);
      const key = nextKey.current++;
      makePane(key, from?.cwd ?? latest.current.home);
      setTabs((list) => list.map((t) => (t.key === tab.key ? { ...t, layout: split(t.layout, tab.focus, dir, key), focus: key } : t)));
    },
    [makePane],
  );

  /** Moves the keys to the nearest pane in `dir`, by where the panes are on screen. */
  const focusToward = useCallback(
    (dir: Direction) => {
      const tab = tabsRef.current.find((t) => t.key === activeRef.current);
      const from = tab && live.current.get(tab.focus)?.wrapper.getBoundingClientRect();
      if (!tab || !from) return;
      const cx = from.left + from.width / 2;
      const cy = from.top + from.height / 2;
      let best: { key: number; d: number } | null = null;
      for (const key of leaves(tab.layout)) {
        if (key === tab.focus) continue;
        const r = live.current.get(key)?.wrapper.getBoundingClientRect();
        if (!r) continue;
        const x = r.left + r.width / 2;
        const y = r.top + r.height / 2;
        const ok = dir === "left" ? x < cx - 1 : dir === "right" ? x > cx + 1 : dir === "up" ? y < cy - 1 : y > cy + 1;
        if (!ok) continue;
        const d = Math.hypot(x - cx, y - cy);
        if (!best || d < best.d) best = { key, d };
      }
      if (best) focusPane(tab.key, best.key);
    },
    [focusPane],
  );

  // The keys a terminal hands to the panel (see panelShortcut), with the latest state.
  const runShortcut = useRef<(s: Shortcut) => void>(() => {});
  runShortcut.current = (s) => {
    const tab = tabsRef.current.find((t) => t.key === activeRef.current);
    if (s.kind === "split") splitFocused(s.dir);
    else if (s.kind === "focus") focusToward(s.dir);
    else if (s.kind === "close") tab && removePane(tab.focus);
    else if (s.kind === "tab") newTab(folder ?? latest.current.home);
  };

  useImperativeHandle(
    ref,
    () => ({
      newTab,
      pastePaths: (paths) => {
        const p = focusedLive;
        if (p?.id != null) void api.termPastePaths(p.id, paths).then(() => p.term.focus());
      },
      focus: () => focusedLive?.term.focus(),
      tabCount: () => count.current,
    }),
    [newTab, focusedLive],
  );

  // Shown again, or another tab in front: fit its panes and give the keys to its pane in front.
  useLayoutEffect(() => {
    if (!visible || !activeTab) return;
    fitAll();
    live.current.get(activeTab.focus)?.term.focus();
    // Hidden, the renderer drew nothing: everything is drawn again at the size it has now.
    requestAnimationFrame(() => {
      for (const key of leaves(activeTab.layout)) {
        const p = live.current.get(key);
        if (p?.opened) p.term.refresh(0, p.term.rows - 1);
      }
    });
    // The tab's identity is enough: a split refits through its leaves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible, active, fitAll]);

  // The panel's size changes with the window and the drag on its top edge.
  useEffect(() => {
    const el = body.current;
    if (!el) return;
    let frame = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(fitAll);
    });
    ro.observe(el);
    return () => {
      cancelAnimationFrame(frame);
      ro.disconnect();
    };
  }, [fitAll]);

  // The accent was just applied to the page: read it after this frame.
  useEffect(() => {
    const frame = requestAnimationFrame(() => {
      for (const p of live.current.values()) p.term.options.theme = themeOf(p);
    });
    return () => cancelAnimationFrame(frame);
  }, [accent, theme]);

  // Tab titles follow the shells' folders (a `cd` changes them), or the
  // server an `ssh` in front of the shell is connected to.
  useEffect(() => {
    if (!visible) return;
    const poll = () => {
      for (const [key, p] of live.current) {
        if (p.id == null) continue;
        api
          .termStatus(p.id)
          .then((st) => {
            if (!st) return;
            const cwd = st.cwd ?? p.cwd;
            // The same program twice in a row (a second and a half): a quick `ls` never repaints the pane.
            const program = st.command != null && st.command === p.command;
            if (program !== p.program) {
              p.program = program;
              watchBackdrop(p);
            }
            if (cwd === p.cwd && st.ssh === p.ssh && st.command === p.command) return;
            p.cwd = cwd;
            p.ssh = st.ssh;
            p.command = st.command;
            setCwds((c) => ({ ...c, [key]: cwd }));
          })
          .catch(() => {});
      }
    };
    poll();
    const timer = window.setInterval(poll, 1500);
    return () => window.clearInterval(timer);
  }, [visible, tabs.length]);

  // The window going away ends every shell.
  useEffect(
    () => () => {
      for (const p of live.current.values()) {
        if (p.id != null) void api.termClose(p.id);
        p.term.dispose();
      }
      live.current.clear();
    },
    [],
  );

  // At most half the room the folders and the panel share: from the folders'
  // top to the bottom of the window's column (its padding aside), which
  // neither the panel's height nor a stored one too tall can change.
  const panel = useRef<HTMLElement | null>(null);
  const [room, setRoom] = useState<number | null>(null);
  useLayoutEffect(() => {
    const el = panel.current;
    const parent = el?.parentElement;
    if (!el || !parent || !visible) return;
    const measure = () => {
      const above = el.previousElementSibling as HTMLElement | null;
      if (!above) return;
      const bottom = parent.getBoundingClientRect().bottom - parseFloat(getComputedStyle(parent).paddingBottom || "0");
      setRoom(bottom - above.getBoundingClientRect().top);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(parent);
    return () => ro.disconnect();
  }, [visible]);
  const maxHeight = room == null ? Infinity : Math.max(TERMINAL_HEIGHT.min, Math.floor(room * TERMINAL_HEIGHT.maxShare));
  const shownHeight = Math.min(height, maxHeight);

  /** Dragging the top edge: the panel grows up, the folders give way (up to half the room). */
  const resize = (ev: React.MouseEvent) => {
    ev.preventDefault();
    const y0 = ev.clientY;
    const h0 = shownHeight;
    const max = maxHeight;
    const move = (m: MouseEvent) => onHeight(Math.round(Math.min(max, Math.max(TERMINAL_HEIGHT.min, h0 + (y0 - m.clientY)))));
    dragging("resizing-row", move);
  };

  /** Dragging a divider: the split's ratio follows the pointer within the split. */
  const dragDivider = (ev: React.MouseEvent, tabKey: number, path: Path, dir: "row" | "column") => {
    ev.preventDefault();
    const box = (ev.currentTarget.parentElement as HTMLElement).getBoundingClientRect();
    const move = (m: MouseEvent) => {
      const ratio = dir === "row" ? (m.clientX - box.left) / box.width : (m.clientY - box.top) / box.height;
      setTabs((list) => list.map((t) => (t.key === tabKey ? { ...t, layout: setRatio(t.layout, path, ratio) } : t)));
    };
    // The panes' observers refit them as they change; once more when the drag ends, to be sure.
    dragging(dir === "row" ? "resizing-col" : "resizing-row", move, () => window.setTimeout(fitAll, 30));
  };

  const titleOf = (t: Tab) => {
    const p = live.current.get(t.focus);
    const cwd = cwds[t.focus] ?? p?.cwd ?? "";
    const n = leaves(t.layout).length;
    // The server over SSH, else the program in front (nvim, htop), else the shell's folder.
    const name = p?.ssh ?? p?.command ?? (baseName(cwd) || tr("terminal.terminal"));
    return `${name}${n > 1 ? ` (${n})` : ""}`;
  };
  /** The tab's tooltip: the server and the title it set, or the program and its folder, or the folder. */
  const hintOf = (t: Tab) => {
    const p = live.current.get(t.focus);
    if (!p) return undefined;
    if (p.ssh) return `${tr("terminal.sshTo", { name: p.ssh })}${p.title ? `\n${p.title}` : ""}`;
    return p.command ? `${p.command}\n${p.cwd}` : p.cwd;
  };

  const renderLayout = (tab: Tab, l: Layout, path: Path): ReactNode => {
    if (isLeaf(l)) {
      const many = !isLeaf(tab.layout);
      const key = l.leaf;
      return (
        <div
          key={`leaf-${key}`}
          className="term-leaf relative min-h-0 min-w-0 flex-1"
          data-dim={many && tab.focus !== key ? "" : undefined}
          onMouseDown={() => focusPane(tab.key, key)}
        >
          {/* The terminal's own element moves in and out of this box (a split never restarts a shell). */}
          <div className="absolute inset-0" ref={(el) => mount(key, el)} />
          {many && (
            <button
              aria-label={tr("terminal.closeThis")}
              title={tr("terminal.closeThis")}
              onMouseDown={(ev) => ev.stopPropagation()}
              onClick={() => setClosing(key)}
              className="term-close absolute right-2 top-1.5 z-10 grid h-6 w-6 place-items-center rounded-full text-ink"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          )}
        </div>
      );
    }
    return (
      <div key={path.join("") || "root"} className={`flex min-h-0 min-w-0 flex-1 ${l.dir === "row" ? "flex-row" : "flex-col"}`}>
        <div className="flex min-h-0 min-w-0" style={{ flex: `${l.ratio} 1 0` }}>
          {renderLayout(tab, l.a, [...path, "a"])}
        </div>
        <div
          className={`term-divider flex-none ${l.dir === "row" ? "w-px cursor-col-resize" : "h-px cursor-row-resize"}`}
          onMouseDown={(ev) => dragDivider(ev, tab.key, path, l.dir)}
        />
        <div className="flex min-h-0 min-w-0" style={{ flex: `${1 - l.ratio} 1 0` }}>
          {renderLayout(tab, l.b, [...path, "b"])}
        </div>
      </div>
    );
  };

  // The pane whose close button asked: confirmed in a dialog.
  const [closing, setClosing] = useState<number | null>(null);
  const closingPane = closing != null ? live.current.get(closing) : undefined;
  const closingWhat = closingPane?.ssh ? tr("terminal.sshSession", { name: closingPane.ssh }) : closingPane?.command ? `"${closingPane.command}"` : null;

  const button = (label: string, title: string, onClick: () => void, icon: ReactNode, disabled = false) => (
    <button
      aria-label={label}
      title={title}
      disabled={disabled}
      onClick={onClick}
      className="grid h-7 w-7 flex-none place-items-center rounded-lg text-dim transition-colors hover:bg-white/8 hover:text-ink disabled:opacity-30"
    >
      {icon}
    </button>
  );

  return (
    <section
      ref={panel}
      // term-dark: the panel keeps the dark look on the light theme.
      className={`glass term-dark relative flex flex-col overflow-hidden ${maximized ? "min-h-0 flex-1" : "mt-4 flex-none"} ${visible ? "" : "hidden"} ${dropHere ? "drop-target" : ""}`}
      style={maximized ? undefined : { height: shownHeight }}
      {...dropAttr("terminal", "terminal")}
    >
      {!maximized && <div className="absolute inset-x-6 -top-1 z-10 h-2.5 cursor-row-resize" onMouseDown={resize} title={tr("terminal.dragResize")} />}
      {/* The pills sit as far from the top edge as from the left: concentric with the panel's corner.
          A double-click on its empty part maximizes or restores the panel, as a window's title bar. */}
      <header
        className="flex h-10 flex-none items-center gap-1 border-b border-line pl-1.5 pr-2"
        onDoubleClick={(e) => {
          const el = e.target as HTMLElement;
          if (!el.closest("button")) onMaximize();
        }}
      >
        <div className="flex min-w-0 flex-1 items-center gap-1.5 overflow-x-auto">
          {tabs.map((t) => {
            const on = t.key === active;
            return (
              <div
                key={t.key}
                className={`group flex h-7 flex-none items-center gap-1.5 rounded-full pl-3 pr-1 text-[0.74rem] transition-colors ${
                  on ? "tab-on text-ink" : "tab-off text-muted hover:text-ink"
                }`}
              >
                <button className="flex max-w-[12rem] items-center gap-1.5" onClick={() => setActive(t.key)} title={hintOf(t)}>
                  {live.current.get(t.focus)?.ssh ? (
                    <Server aria-label={tr("terminal.ssh")} className={`h-3.5 w-3.5 flex-none ${on ? "accent-fg" : ""}`} strokeWidth={2} />
                  ) : (
                    <SquareTerminal className={`h-3.5 w-3.5 flex-none ${on ? "accent-fg" : ""}`} strokeWidth={2} />
                  )}
                  <span className="truncate">{titleOf(t)}</span>
                </button>
                <button
                  aria-label={tr("terminal.close")}
                  title={tr("terminal.close")}
                  onClick={() => {
                    for (const key of leaves(t.layout)) removePane(key);
                  }}
                  className={`grid h-5 w-5 place-items-center rounded-full text-dim hover:bg-white/10 hover:text-ink ${on ? "" : "opacity-0 group-hover:opacity-100"}`}
                >
                  <X className="h-3 w-3" />
                </button>
              </div>
            );
          })}
          <button
            aria-label={tr("terminal.new")}
            title={`${tr("terminal.new")} (${isMac ? "Cmd+T" : "Ctrl+Shift+T"})`}
            onClick={() => newTab(folder ?? home)}
            className="tab-add grid h-7 w-7 flex-none place-items-center rounded-full text-muted transition-colors hover:text-ink"
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
        </div>
        {button(
          tr("terminal.splitRight"),
          `${tr("terminal.splitRight")} (${isMac ? "Cmd+D" : "Ctrl+Shift+E"})`,
          () => splitFocused("row"),
          <Columns2 className="h-3.5 w-3.5" />,
          !activeTab,
        )}
        {button(
          tr("terminal.splitDown"),
          `${tr("terminal.splitDown")} (${isMac ? "Cmd+Shift+D" : "Ctrl+Shift+O"})`,
          () => splitFocused("column"),
          <Rows2 className="h-3.5 w-3.5" />,
          !activeTab,
        )}
        {button(
          tr("terminal.goToFolder"),
          folder ? tr("terminal.cdTo", { name: baseName(folder) }) : tr("terminal.noFolder"),
          () => {
            if (folder && focusedLive?.id != null) void api.termCd(focusedLive.id, folder).then(() => focusedLive.term.focus());
          },
          <FolderInput className="h-3.5 w-3.5" />,
          !folder || focusedLive?.id == null,
        )}
        {button(
          maximized ? tr("terminal.restore") : tr("terminal.maximize"),
          maximized ? tr("terminal.restore") : tr("terminal.maximize"),
          onMaximize,
          maximized ? <Minimize2 className="h-3.5 w-3.5" /> : <Maximize2 className="h-3.5 w-3.5" />,
        )}
        {button(tr("terminal.hide"), `${tr("terminal.hide")} (${TERMINAL_KEYS})`, onHide, <ChevronDown className="h-4 w-4" />)}
      </header>
      <div ref={body} className="terminal-body relative min-h-0 flex-1">
        {tabs.map((t) => (
          <div key={t.key} className="absolute inset-0 flex" style={{ visibility: t.key === active ? "visible" : "hidden" }}>
            {renderLayout(t, t.layout, [])}
          </div>
        ))}
      </div>
      {closingPane && closing != null && (
        <Modal onCancel={() => setClosing(null)}>
          <Heading
            icon={<SquareTerminal className="h-4.5 w-4.5 text-muted" />}
            title={tr("terminal.closeAsk")}
            subtitle={closingWhat ? tr("terminal.closeRunning", { what: closingWhat }) : tr("terminal.closeShell")}
          />
          <div className="mt-5 flex justify-end gap-2">
            <button type="button" className="btn btn-ghost" onClick={() => setClosing(null)} autoFocus>
              {tr("common.cancel")}
            </button>
            <button
              type="button"
              className="btn btn-danger"
              onClick={() => {
                const key = closing;
                setClosing(null);
                removePane(key);
              }}
            >
              {tr("terminal.closeButton")}
            </button>
          </div>
        </Modal>
      )}
    </section>
  );
});

/** What a key does in the panel, from inside a terminal (null: the shell's). */
type Shortcut = { kind: "split"; dir: "row" | "column" } | { kind: "focus"; dir: Direction } | { kind: "close" } | { kind: "tab" };

const ARROWS: Record<string, Direction> = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };

/**
 * iTerm's keys on macOS: Cmd+D splits right, Cmd+Shift+D down,
 * Cmd+Option+arrows move between panes, Cmd+W closes one, Cmd+T opens a tab.
 * Terminator's on Linux: Ctrl+Shift+E right, Ctrl+Shift+O down, Alt+arrows,
 * Ctrl+Shift+W and Ctrl+Shift+T.
 */
function panelShortcut(e: KeyboardEvent): Shortcut | null {
  const k = e.key.toLowerCase();
  if (isMac) {
    if (!e.metaKey) return null;
    if (e.code === "KeyD") return { kind: "split", dir: e.shiftKey ? "column" : "row" };
    if (e.altKey && ARROWS[e.key]) return { kind: "focus", dir: ARROWS[e.key] };
    if (k === "w" && !e.shiftKey) return { kind: "close" };
    if (k === "t" && !e.shiftKey) return { kind: "tab" };
    return null;
  }
  if (e.ctrlKey && e.shiftKey) {
    if (e.code === "KeyE") return { kind: "split", dir: "row" };
    if (e.code === "KeyO") return { kind: "split", dir: "column" };
    if (e.code === "KeyW") return { kind: "close" };
    if (e.code === "KeyT") return { kind: "tab" };
  }
  if (e.altKey && !e.ctrlKey && !e.shiftKey && ARROWS[e.key]) return { kind: "focus", dir: ARROWS[e.key] };
  return null;
}

/** Follows the pointer until the button is let go, with the cursor of the drag everywhere. */
function dragging(cursorClass: string, move: (m: MouseEvent) => void, done?: () => void) {
  const up = () => {
    window.removeEventListener("mousemove", move);
    window.removeEventListener("mouseup", up);
    document.documentElement.classList.remove(cursorClass);
    done?.();
  };
  document.documentElement.classList.add(cursorClass);
  window.addEventListener("mousemove", move);
  window.addEventListener("mouseup", up);
}
