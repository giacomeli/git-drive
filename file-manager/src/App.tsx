import { BookmarkMinus, BookmarkPlus, Columns2, Folder, FolderOpen, PanelTop, Plus, X } from "lucide-react";
import { Fragment, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api, baseName, errorText, parentOf, type Boot, type Entry, type Place, type SidebarData } from "./lib/api";
import { activeItem, COMPUTER, isWithin, rebase } from "./lib/nav";
import { isMac } from "./components/Chrome";
import { ContextMenu, type MenuItem } from "./components/ContextMenu";
import { FolderPane, type PaneEnv, type PaneHandle } from "./components/FolderPane";
import { Sidebar } from "./components/Sidebar";
import { TaskDetails, TaskTray } from "./components/Tasks";
import { TERMINAL_HEIGHT, TerminalPanel, type TerminalHandle } from "./components/TerminalPanel";
import { DndProvider, dropAttr, useDragAndDrop } from "./lib/dnd";
import { CutProvider, RenameProvider, useFileOps } from "./lib/fileops";
import { configureFolderPreviews, invalidateFolderPreviews } from "./lib/useFolderPreview";
import { isSplitKey, isTerminalKey } from "./lib/shortcuts";
import { useStored } from "./lib/stored";
import { dragDivider, evenShares, fits, MAX_PANES, removeAt } from "./lib/shares";
// `tr`: the message function (`t` is a tab all over this file).
import { t as tr, useI18n } from "./i18n";
import { applyAppearance, onSystemChange, type Appearance, type Theme } from "./lib/theme";
import { useToast } from "./ui/ui";
import { AboutDialog } from "./components/AboutDialog";
import { PropertiesDialog } from "./components/PropertiesDialog";
import { SettingsDialog, type WindowPrefs } from "./components/SettingsDialog";
import { applyAccent } from "./ui/accent";

/** How often the sidebar looks again for volumes mounted or ejected meanwhile. */
const SIDEBAR_POLL_MS = 10_000;
/** How long a drag rests on a tab before the tab opens (Finder's spring-loading). */
const SPRING_MS = 650;
/** Hover time before a tab shows its folder's full path. */
const TAB_TIP_MS = 450;

/** A folder pane of a tab, and the folder it opened at. */
interface PaneModel {
  id: number;
  start: string;
}

/** A tab: one pane, or up to `MAX_PANES` side by side, and the one with the keys. */
interface TabModel {
  id: number;
  panes: PaneModel[];
  focus: number;
  /** Each pane's share of the width when split (even until a divider is dragged). */
  shares?: number[];
}

/** The narrowest a pane of a split gets: a new pane is refused below it, and a divider stops there. */
const MIN_PANE = 280;
/** A divider's width (`w-4`). */
const DIVIDER = 16;

/**
 * The window: the sidebar, tabs of one to four folder panes side by side
 * (Dolphin's split view, with more panes), the terminal, Settings, and what the panes share (the bookmarks,
 * the file operations and their clipboard, drag and drop). The pane with
 * the keys draws the toolbar and the preview in the window's slots.
 */
export function App({ boot }: { boot: Boot }) {
  // Read here so the whole window renders again when the language changes.
  const { language } = useI18n();
  const toast = useToast();
  // Settings and About open over the window, as Quick Look does.
  const [modal, setModal] = useState<"settings" | "about" | null>(null);
  const [accent, setAccent] = useState(boot.accent);
  const [collapsed, setCollapsed] = useStored("sidebarCollapsed", false);
  const [showPreview, setShowPreview] = useStored("preview", true);
  // The look: as the system has it, or one forced (Settings); the theme it shows now.
  const [appearance, setAppearance] = useStored<Appearance>("appearance", "system");
  const [theme, setTheme] = useState<Theme>(() => (document.documentElement.dataset.theme === "light" ? "light" : "dark"));
  useEffect(() => {
    setTheme(applyAppearance(appearance));
    if (appearance !== "system") return;
    return onSystemChange(() => setTheme(applyAppearance("system")));
  }, [appearance]);
  // The status bar under each pane (Settings).
  const [statusBar, setStatusBar] = useStored("statusBar", true);
  // The map in a photo's Properties (Settings): it loads OpenStreetMap's tiles.
  const [photoMap, setPhotoMap] = useStored("photoMap", true);
  const [folderPreviews, setFolderPreviews] = useStored("folderPreviews", true);
  const [folderPreviewsDrive, setFolderPreviewsDrive] = useStored("folderPreviewsDrive", true);
  const drivePath = boot.places.find((p) => p.icon === "drive")?.path ?? null;
  useLayoutEffect(() => {
    configureFolderPreviews({ enabled: folderPreviews, onDrive: folderPreviewsDrive, drive: drivePath });
  }, [folderPreviews, folderPreviewsDrive, drivePath]);
  // The items whose Properties are open.
  const [propsFor, setPropsFor] = useState<string[] | null>(null);
  // The terminal panel: shown or not, its height, and its tabs (owned by the panel).
  const [terminalShown, setTerminalShown] = useStored("terminal", false);
  const [terminalHeight, setTerminalHeight] = useStored("terminalHeight", TERMINAL_HEIGHT.initial);
  // The terminal over the folders (kept while hidden: shown again, it comes back maximized, as VS Code's panel).
  const [terminalMax, setTerminalMax] = useState(false);
  const maxed = terminalMax && terminalShown;
  const terminal = useRef<TerminalHandle | null>(null);
  const [side, setSide] = useState<SidebarData | null>(null);
  // This app's bundle (macOS), to find it when adding it to Full Disk Access.
  const [appBundle, setAppBundle] = useState<string | null>(null);
  // The sidebar's menus (the panes keep their own).
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  // Where the pane with the keys draws the toolbar and the preview.
  const [toolbarSlot, setToolbarSlot] = useState<HTMLElement | null>(null);
  const [previewSlot, setPreviewSlot] = useState<HTMLElement | null>(null);

  // --- tabs and their panes ---
  const nextId = useRef(3);
  const [tabs, setTabs] = useState<TabModel[]>(() => [{ id: 1, panes: [{ id: 2, start: boot.start }], focus: 2 }]);
  const [activeTab, setActiveTab] = useState(1);
  // Where each pane is, as it reports it.
  const [locations, setLocations] = useState<Record<number, string>>({});
  const handles = useRef(new Map<number, PaneHandle>());
  const tab = tabs.find((t) => t.id === activeTab) ?? tabs[0];
  const focusedPane = tab.focus;
  const locationOf = (t: TabModel, paneId = t.focus) => locations[paneId] ?? t.panes.find((p) => p.id === paneId)?.start ?? boot.start;
  const focusedLocation = locationOf(tab);
  const latest = useRef({ tabs, tab, focusedPane, locations });
  latest.current = { tabs, tab, focusedPane, locations };
  const focusedHandle = () => handles.current.get(latest.current.focusedPane);

  const trashDir = side?.trash ?? null;
  const home = boot.places.find((p) => p.icon === "home")?.path ?? "/";
  // Where the columns and the path bar start: the drive, the home folder, the volumes (as mounted now) and the Trash.
  const anchors = useMemo<Place[]>(() => {
    if (!side) return boot.places;
    const volumes: Place[] = side.volumes
      .filter((v) => v.kind !== "drive")
      .map((v) => ({ label: v.name, path: v.path, icon: "disk", missing: false }));
    const trash: Place = { label: tr("window.trash"), path: side.trash, icon: "trash", missing: false };
    return [...boot.places.filter((p) => p.icon === "drive" || p.icon === "home"), ...volumes, trash];
    // The Trash's label follows the language.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [side, boot.places, language]);
  const bookmarkPaths = useMemo(() => side?.bookmarks.map((b) => b.path) ?? [], [side]);

  const newTab = useCallback((path?: string) => {
    const id = nextId.current++;
    const paneId = nextId.current++;
    const { tab: current, locations: locs } = latest.current;
    const start = path ?? locs[current.focus] ?? current.panes[0].start;
    setTabs((list) => [...list, { id, panes: [{ id: paneId, start }], focus: paneId }]);
    setActiveTab(id);
  }, []);

  const closeTab = useCallback((id: number) => {
    const list = latest.current.tabs;
    if (list.length === 1) return;
    const at = list.findIndex((t) => t.id === id);
    const rest = list.filter((t) => t.id !== id);
    setTabs(rest);
    setActiveTab((cur) => (cur === id ? rest[Math.min(at, rest.length - 1)].id : cur));
  }, []);

  const stepTab = (delta: number) => {
    const i = tabs.findIndex((t) => t.id === tab.id);
    setActiveTab(tabs[(i + delta + tabs.length) % tabs.length].id);
  };

  const focusPane = useCallback((tabId: number, paneId: number) => {
    setTabs((list) => list.map((t) => (t.id === tabId && t.focus !== paneId ? { ...t, focus: paneId } : t)));
  }, []);

  /** The width the panes of tab `id` share (its dividers included). */
  const tabBoxes = useRef(new Map<number, HTMLDivElement>());
  const widthOf = (id: number) => tabBoxes.current.get(id)?.clientWidth ?? 0;

  /** Adds a pane at `start` right after the focused one of the tab in front, with the keys; refused (and said
   * why) at `MAX_PANES` or when a pane would get narrower than `MIN_PANE`. */
  const addPane = useCallback((start: string) => {
    const { tab: current } = latest.current;
    const n = current.panes.length;
    if (n >= MAX_PANES) return void toast("error", tr("window.splitFull", { count: MAX_PANES }));
    const width = widthOf(current.id);
    if (width > 0 && !fits(n + 1, width, MIN_PANE, DIVIDER)) return void toast("error", tr("window.splitNoRoom", { width: MIN_PANE }));
    const paneId = nextId.current++;
    setTabs((list) =>
      list.map((t) => {
        if (t.id !== current.id) return t;
        const at = Math.max(0, t.panes.findIndex((p) => p.id === t.focus));
        const panes = [...t.panes.slice(0, at + 1), { id: paneId, start }, ...t.panes.slice(at + 1)];
        // A pane added evens them out: each new folder gets as much room as the others.
        return { ...t, panes, focus: paneId, shares: evenShares(panes.length) };
      }),
    );
    // `toast` is stable; `latest` holds the rest.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Splits the tab in front (a second pane at the same folder, with the keys), or keeps only the pane with them. */
  const toggleSplit = useCallback(() => {
    const { tab: current, locations: locs } = latest.current;
    if (current.panes.length > 1) {
      setTabs((list) => list.map((t) => (t.id === current.id ? { ...t, panes: t.panes.filter((p) => p.id === t.focus), shares: undefined } : t)));
      return;
    }
    addPane(locs[current.focus] ?? current.panes[0].start);
  }, [addPane]);

  /** Shows `path` in a new pane beside the focused one of the tab in front (splitting it first when it has one). */
  const openInSplit = useCallback((path: string) => addPane(path), [addPane]);

  /** Closes a pane of a split; its neighbors take its room, and the keys go to the one on its left (or right). */
  const closePane = useCallback((tabId: number, paneId: number) => {
    setTabs((list) =>
      list.map((t) => {
        if (t.id !== tabId || t.panes.length < 2) return t;
        const at = t.panes.findIndex((p) => p.id === paneId);
        const panes = t.panes.filter((p) => p.id !== paneId);
        const shares = panes.length > 1 ? removeAt(t.shares ?? evenShares(t.panes.length), at) : undefined;
        const focus = t.focus === paneId ? panes[Math.max(0, at - 1)].id : t.focus;
        return { ...t, panes, focus, shares };
      }),
    );
  }, []);

  /** The divider after pane `index` of tab `tabId` dragged to `x` (a fraction of the panes' width), or (null) all evened out. */
  const moveDivider = useCallback((tabId: number, index: number, x: number | null) => {
    setTabs((list) =>
      list.map((t) => {
        if (t.id !== tabId) return t;
        if (x == null) return t.shares ? { ...t, shares: undefined } : t;
        const n = t.panes.length;
        const inner = widthOf(tabId) - (n - 1) * DIVIDER;
        const least = inner > 0 ? MIN_PANE / inner : 0;
        return { ...t, shares: dragDivider(t.shares ?? evenShares(n), index, x, least) };
      }),
    );
  }, []);

  /** Tab (Shift+Tab backwards), in a split: the keys go to the next pane. */
  const switchPane = (step: number) => {
    const n = tab.panes.length;
    if (n < 2) return;
    const at = tab.panes.findIndex((p) => p.id === tab.focus);
    focusPane(tab.id, tab.panes[(at + step + n) % n].id);
  };

  const paneRefs = useRef(new Map<number, (h: PaneHandle | null) => void>());
  const paneRef = (id: number) => {
    let fn = paneRefs.current.get(id);
    if (!fn) {
      fn = (h) => {
        if (h) handles.current.set(id, h);
        else handles.current.delete(id);
      };
      paneRefs.current.set(id, fn);
    }
    return fn;
  };

  /** Goes to `path` in the pane with the keys (the sidebar, the terminal's links). */
  const goTo = (path: string, selected: string | null = null) => {
    focusedHandle()?.navigate(path, selected);
  };

  // --- the file operations, shared by the panes (one clipboard) ---
  const everyPane = () => [...handles.current.values()];
  const ops = useFileOps({
    toast,
    onChanged: () => {
      everyPane().forEach((h) => h.refresh());
      invalidateFolderPreviews("op");
    },
    onLanded: (paths) => {
      const dir = paths[0] ? parentOf(paths[0]) : null;
      if (!dir) return;
      // Selected where they show; else in the pane with the keys.
      const showing = everyPane().filter((h) => h.shows(dir));
      for (const h of showing.length > 0 ? showing : [focusedHandle()].filter((h) => h != null)) h.landed(paths);
    },
    onRemoved: (paths) => everyPane().forEach((h) => h.removed(paths)),
    onRenamed: (from, to) => {
      everyPane().forEach((h) => h.renamed(from, to));
      setTabs((list) => list.map((t) => ({ ...t, panes: t.panes.map((p) => ({ ...p, start: rebase(p.start, from, to) })) })));
      // Bookmarks follow a folder renamed here (or one they live in).
      if (bookmarkPaths.some((p) => rebase(p, from, to) !== p)) saveBookmarks(bookmarkPaths.map((p) => rebase(p, from, to)));
      const name = baseName(to);
      if (name.startsWith(".") && !focusedHandle()?.showsHidden()) {
        toast("ok", tr("window.hiddenNow", { name, keys: `${isMac ? "Cmd" : "Ctrl"}+Shift+.` }));
      }
    },
  });

  /** Why an item stays where it is (not dragged, renamed or deleted from here), or null when it can move. */
  const fixedReason = (entry: Entry): string | null => {
    const p = entry.path;
    const name = entry.name;
    if (p === home) return tr("window.fixed.home");
    if (p === boot.places.find((x) => x.icon === "drive")?.path) return tr("window.fixed.drive", { name });
    if (trashDir && (p === trashDir || isWithin(trashDir, p)) && boot.fixed.includes(p)) return tr("window.fixed.trash");
    if (anchors.some((a) => a.path === p)) return tr("window.fixed.volume", { name });
    if (parentOf(p) === home && boot.fixed.includes(p)) return tr("window.fixed.standard", { name });
    if (parentOf(p) === "/") return tr("window.fixed.system", { name });
    if (boot.fixed.includes(p)) return tr("window.fixed.other", { name });
    return null;
  };

  // --- bookmarks ---
  const loadSidebar = useCallback(() => {
    api
      .sidebar()
      .then(setSide)
      .catch(() => {});
  }, []);
  const saveBookmarks = (paths: string[]) => {
    // Shown at once; the backend's answer names them.
    setSide((s) => s && { ...s, bookmarks: paths.map((p) => s.bookmarks.find((b) => b.path === p) ?? { label: baseName(p), path: p, icon: "folder", missing: false }) });
    api
      .saveBookmarks(paths)
      .then((bookmarks) => setSide((s) => s && { ...s, bookmarks }))
      .catch((e) => {
        toast("error", errorText(e));
        loadSidebar();
      });
  };
  /** Bookmarks `paths` (folders only) at `at`, or last; one already there moves. */
  const addBookmarks = async (paths: string[], at?: number) => {
    const kinds = await Promise.all(paths.map((p) => api.previewKind(p).catch(() => "other")));
    const folders = paths.filter((_, i) => kinds[i] === "directory");
    if (folders.length < paths.length) toast("error", tr("window.onlyFolders"));
    if (folders.length === 0) return;
    const current = bookmarkPaths;
    let idx = Math.min(at ?? current.length, current.length);
    idx -= current.slice(0, idx).filter((p) => folders.includes(p)).length;
    const next = current.filter((p) => !folders.includes(p));
    next.splice(idx, 0, ...folders);
    saveBookmarks(next);
  };
  const removeBookmark = (path: string) => saveBookmarks(bookmarkPaths.filter((p) => p !== path));
  const bookmarkItem = (path: string): MenuItem =>
    bookmarkPaths.includes(path)
      ? { label: tr("window.removeBookmark"), Icon: BookmarkMinus, run: () => removeBookmark(path) }
      : { label: tr("window.addBookmark"), Icon: BookmarkPlus, run: () => void addBookmarks([path]) };

  /** A bookmark's menu; a missing folder can only go. */
  const bookmarkMenu = (ev: React.MouseEvent, place: Place) => {
    const items: MenuItem[] = place.missing
      ? []
      : [
          { label: tr("common.open"), Icon: FolderOpen, run: () => goTo(place.path) },
          { label: tr("window.openInNewTab"), Icon: PanelTop, run: () => newTab(place.path) },
          { label: tr("window.openInSplit"), Icon: Columns2, run: () => openInSplit(place.path) },
          "separator",
        ];
    items.push({ label: tr("window.removeBookmark"), Icon: BookmarkMinus, run: () => removeBookmark(place.path) });
    setMenu({ x: ev.clientX, y: ev.clientY, items });
  };

  // --- drag and drop ---
  const dnd = useDragAndDrop({
    onRefused: (reason) => toast("error", reason),
    onDrop: (paths, spot) => {
      if (spot.kind === "bookmark") void addBookmarks(paths, Number(spot.dir));
      else if (spot.kind === "trash") {
        const away = paths.filter((p) => !(trashDir && isWithin(p, trashDir)));
        if (away.length > 0) ops.trashNow(away);
      } else if (spot.kind === "terminal") terminal.current?.pastePaths(paths);
      else if (spot.kind === "tab") {
        // A tab takes them into the folder of its pane with the keys.
        const t = latest.current.tabs.find((x) => String(x.id) === spot.dir);
        const dir = t && (latest.current.locations[t.focus] ?? t.panes.find((p) => p.id === t.focus)?.start);
        if (dir && dir !== COMPUTER) ops.drop(paths, dir);
      } else ops.drop(paths, spot.dir);
    },
    // The other app finishes its move on its own time: look again shortly, and again later.
    onDraggedAway: () => {
      for (const ms of [400, 1500])
        window.setTimeout(() => {
          everyPane().forEach((h) => h.refresh());
          invalidateFolderPreviews("op");
        }, ms);
    },
    onError: (text) => toast("error", text),
  });

  // A drag resting on a tab opens it.
  useEffect(() => {
    const t = dnd.target;
    if (!t?.startsWith("tab:")) return;
    const id = Number(t.slice("tab:".length));
    if (id === activeTab) return;
    const timer = window.setTimeout(() => setActiveTab(id), SPRING_MS);
    return () => window.clearTimeout(timer);
  }, [dnd.target, activeTab]);

  // --- the file operations' details (the eye, or the collapsed sidebar's ring) ---
  const [details, setDetails] = useState<number[] | null>(null);
  const showDetails = (ids: number[]) => {
    setDetails(ids);
    ops.viewTasks(ids);
  };
  const closeDetails = () => {
    setDetails(null);
    ops.viewTasks([]);
  };

  // --- a tab's full path, on hover ---
  const [tabTip, setTabTip] = useState<{ id: number; anchor: DOMRect } | null>(null);
  const tabTipTimer = useRef<number | undefined>(undefined);
  const hideTabTip = useCallback(() => {
    window.clearTimeout(tabTipTimer.current);
    setTabTip(null);
  }, []);
  // A drag, a click or a key ends it: the tab may change under it.
  useEffect(hideTabTip, [dnd.target, activeTab, hideTabTip]);

  // --- the terminal ---
  function toggleTerminal() {
    setTerminalShown((s) => !s);
  }
  /** A new terminal tab in `path` (Open in Terminal). */
  function openTerminalAt(path: string) {
    setTerminalShown(true);
    terminal.current?.newTab(path);
  }
  // Where a new terminal starts: the folder in view (the Computer view has none).
  const terminalFolder = focusedLocation === COMPUTER ? null : focusedLocation;
  // The terminal shown with no tab (Cmd+J, or shown when the app opened): one in the folder in view.
  useEffect(() => {
    if (terminalShown && terminal.current?.tabCount() === 0) terminal.current.newTab(terminalFolder ?? home);
    // Only on showing: a later folder change is not a reason for a new tab.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [terminalShown]);

  // --- the sidebar's bookmarks and volumes; the accent; the app bundle ---
  useEffect(() => {
    loadSidebar();
    // Volumes come and go (a stick plugged in, a drive ejected).
    const timer = window.setInterval(loadSidebar, SIDEBAR_POLL_MS);
    window.addEventListener("focus", loadSidebar);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", loadSidebar);
    };
  }, [loadSidebar]);
  // The accent is shared with the Control Center: a change made there shows
  // here when this window comes back to the front.
  useEffect(() => {
    applyAccent(accent);
  }, [accent]);
  useEffect(() => {
    const onFocus = () => {
      api
        .accent()
        .then(setAccent)
        .catch(() => {});
      // Files may have changed in other apps meanwhile.
      invalidateFolderPreviews("focus");
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);
  useEffect(() => {
    if (boot.platform === "macOS")
      api
        .appBundle()
        .then(setAppBundle)
        .catch(() => {});
  }, [boot.platform]);

  // The webview's own menu (Reload, Inspect) never shows; text keeps its Copy.
  useEffect(() => {
    const onMenu = (e: MouseEvent) => {
      if (!(e.target as HTMLElement | null)?.closest("input, textarea, .selectable")) e.preventDefault();
    };
    window.addEventListener("contextmenu", onMenu);
    return () => window.removeEventListener("contextmenu", onMenu);
  }, []);

  // --- the window's keys, ahead of the pane's (capture): Settings, the
  // sidebar, the terminal, tabs and splits ---
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // The menus and the dialogs own the keyboard while open.
      if (document.querySelector('[role="menu"], [role="dialog"]')) return;
      const target = e.target as HTMLElement | null;
      const handled = () => {
        e.preventDefault();
        e.stopPropagation();
      };
      // The terminal, as VS Code's panel: Cmd+J (Ctrl+J) anywhere, from inside it too.
      if (isTerminalKey(e)) return void (handled(), toggleTerminal());
      // In a terminal, every other key is the shell's (the panel takes its own: tabs, splits).
      if (target?.closest?.(".xterm")) return;
      const cmd = isMac ? e.metaKey : e.ctrlKey;
      if ((e.metaKey || e.ctrlKey) && e.key === ",") return void (handled(), setModal("settings"));
      if (isMac ? e.metaKey && e.ctrlKey && e.key.toLowerCase() === "s" : e.key === "F9") return void (handled(), setCollapsed((c) => !c));
      // Tabs: Cmd+T, Cmd+W, Ctrl+Tab and Cmd+Shift+[ ], Cmd+1 to 9 (9: the last one).
      if (cmd && !e.shiftKey && !e.altKey && e.code === "KeyT") return void (handled(), newTab());
      if (cmd && !e.shiftKey && !e.altKey && e.code === "KeyW") return void (handled(), closeTab(tab.id));
      if (e.ctrlKey && e.key === "Tab") return void (handled(), stepTab(e.shiftKey ? -1 : 1));
      if (cmd && e.shiftKey && (e.code === "BracketRight" || e.code === "BracketLeft")) return void (handled(), stepTab(e.code === "BracketRight" ? 1 : -1));
      if (cmd && !e.shiftKey && !e.altKey && /^Digit[1-9]$/.test(e.code)) {
        handled();
        const n = Number(e.code.slice("Digit".length));
        setActiveTab((n === 9 ? tabs[tabs.length - 1] : (tabs[n - 1] ?? tabs[tabs.length - 1])).id);
        return;
      }
      // Split view (Cmd+/, Ctrl+/), and Tab between its panes.
      if (isSplitKey(e)) return void (handled(), toggleSplit());
      const typing = target?.tagName === "INPUT" || target?.tagName === "TEXTAREA";
      if (e.key === "Tab" && !e.metaKey && !e.ctrlKey && !e.altKey && !typing && tab.panes.length > 1) return void (handled(), switchPane(e.shiftKey ? -1 : 1));
      // Finder's Show Preview.
      if (cmd && e.shiftKey && e.code === "KeyP") return void (handled(), setShowPreview((p) => !p));
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  const labelOf = (path: string) =>
    path === COMPUTER ? tr("window.computer") : path === trashDir ? tr("window.trash") : (anchors.find((a) => a.path === path)?.label ?? baseName(path));
  const tipTab = tabTip ? tabs.find((t) => t.id === tabTip.id) : undefined;
  const titleOf = (t: TabModel) => t.panes.map((p) => labelOf(locationOf(t, p.id))).join(" | ");

  const env: PaneEnv = {
    boot,
    toast,
    anchors,
    trashDir,
    ops,
    dnd,
    fixedReason,
    bookmarkItem,
    openTerminalAt,
    terminalShown,
    toggleTerminal,
    showPreview,
    togglePreview: () => setShowPreview((p) => !p),
    toggleSplit,
    newTab: () => newTab(),
    openInNewTab: (path) => newTab(path),
    openInSplit,
    openSettings: () => setModal("settings"),
    openAbout: () => setModal("about"),
    openProperties: (paths) => setPropsFor(paths),
    showStatusBar: statusBar,
    toggleStatusBar: () => setStatusBar((v) => !v),
    appBundle,
    sidebarCollapsed: collapsed,
    toolbarSlot,
    previewSlot,
  };

  return (
    <CutProvider value={ops.cutPaths}>
      <RenameProvider value={ops.renaming}>
        <DndProvider value={dnd}>
          <div className="surface flex">
            <div className="orb" style={{ width: 380, height: 380, left: 160, top: 140, background: "var(--a1)" }} />
            <div className="orb" style={{ width: 320, height: 320, right: 80, bottom: 40, background: "var(--a3)", opacity: 0.35 }} />
            <div className="orb" style={{ width: 260, height: 260, right: 380, top: 30, background: "var(--s1)", opacity: 0.22 }} />

            <Sidebar
              collapsed={collapsed}
              bookmarks={side?.bookmarks ?? []}
              volumes={side?.volumes ?? []}
              trash={trashDir}
              active={activeItem(
                focusedLocation,
                bookmarkPaths,
                (side?.volumes ?? []).map((v) => v.path),
                trashDir,
              )}
              onOpen={(p) => goTo(p)}
              onReorder={saveBookmarks}
              onBookmarkMenu={bookmarkMenu}
              target={dnd.target}
              onToggle={() => setCollapsed((c) => !c)}
              footer={
                ops.tasks.length > 0 ? (
                  <TaskTray tasks={ops.tasks} collapsed={collapsed} onCancel={ops.cancelTask} onDismiss={ops.dismissTask} onDetails={showDetails} />
                ) : null
              }
            />

            {/* Kept while Settings shows: the panes and the terminal's shells live in it. */}
            <main className="relative z-10 flex min-w-0 flex-1 flex-col pb-4 pr-4">
              <div ref={setToolbarSlot} className="contents" />
              {tabs.length > 1 && !maxed && (
                <div className="mb-3 flex h-8 flex-none items-center gap-1.5">
                  {tabs.map((t) => {
                    const on = t.id === tab.id;
                    const target = dnd.target === `tab:${t.id}`;
                    return (
                      <div
                        key={t.id}
                        {...dropAttr(String(t.id), "tab")}
                        onClick={() => setActiveTab(t.id)}
                        onMouseDown={(ev) => {
                          hideTabTip();
                          // The middle button closes a tab, as in a browser.
                          if (ev.button === 1) {
                            ev.preventDefault();
                            closeTab(t.id);
                          }
                        }}
                        onMouseEnter={(ev) => {
                          const el = ev.currentTarget;
                          window.clearTimeout(tabTipTimer.current);
                          tabTipTimer.current = window.setTimeout(() => setTabTip({ id: t.id, anchor: el.getBoundingClientRect() }), TAB_TIP_MS);
                        }}
                        onMouseLeave={hideTabTip}
                        className={`group relative flex h-8 min-w-0 max-w-[18rem] flex-1 basis-0 cursor-default items-center gap-2 rounded-full pl-3.5 pr-1.5 text-[0.76rem] transition-colors ${
                          target ? "drop-target" : on ? "tab-on text-ink" : "tab-off text-muted hover:text-ink"
                        }`}
                      >
                        {t.panes.length > 1 ? (
                          <Columns2 className={`h-3.5 w-3.5 flex-none ${on ? "accent-fg" : ""}`} />
                        ) : (
                          <Folder className={`h-3.5 w-3.5 flex-none ${on ? "accent-fg" : ""}`} />
                        )}
                        <span className="min-w-0 flex-1 truncate">{titleOf(t)}</span>
                        <button
                          aria-label={tr("window.closeTab")}
                          title={`${tr("window.closeTab")} (${isMac ? "Cmd+W" : "Ctrl+W"})`}
                          onClick={(ev) => {
                            ev.stopPropagation();
                            closeTab(t.id);
                          }}
                          className={`grid h-5 w-5 flex-none place-items-center rounded-full text-dim hover:bg-white/10 hover:text-ink ${on ? "" : "opacity-0 group-hover:opacity-100"}`}
                        >
                          <X className="h-3 w-3" />
                        </button>
                      </div>
                    );
                  })}
                  <button
                    aria-label={tr("window.newTab")}
                    title={`${tr("window.newTab")} (${isMac ? "Cmd+T" : "Ctrl+T"})`}
                    onClick={() => newTab()}
                    className="tab-add grid h-8 w-8 flex-none place-items-center rounded-full text-muted transition-colors hover:text-ink"
                  >
                    <Plus className="h-3.5 w-3.5" />
                  </button>
                </div>
              )}
              {/* Hidden, not unmounted, under a maximized terminal: the panes keep their state. */}
              <div className={`flex min-h-0 flex-1 gap-4 ${maxed ? "hidden" : ""}`}>
                {tabs.map((t) => (
                  <div
                    key={t.id}
                    ref={(el) => {
                      if (el) tabBoxes.current.set(t.id, el);
                      else tabBoxes.current.delete(t.id);
                    }}
                    className={`flex min-w-0 flex-1 ${t.id === tab.id ? "" : "hidden"}`}
                  >
                    {t.panes.map((p, i) => (
                      <Fragment key={p.id}>
                        {i > 0 && <SplitDivider index={i - 1} panes={t.panes.length} onMove={(x) => moveDivider(t.id, i - 1, x)} />}
                        {/* Its share of the width (even until a divider is dragged). */}
                        <div className="flex min-w-0" style={{ flex: `${t.panes.length > 1 ? (t.shares?.[i] ?? 1 / t.panes.length) : 1} 1 0%` }}>
                      <FolderPane
                        key={p.id}
                        ref={paneRef(p.id)}
                        start={p.start}
                        env={env}
                        active={t.id === tab.id && t.focus === p.id}
                        split={t.panes.length > 1}
                        focused={t.focus === p.id}
                        onFocus={() => focusPane(t.id, p.id)}
                        onClose={() => closePane(t.id, p.id)}
                        onLocation={(path) => {
                          // Going somewhere (the sidebar, the toolbar) under a maximized terminal shows the folders again.
                          if (locations[p.id] !== undefined && locations[p.id] !== path && t.focus === p.id) setTerminalMax(false);
                          setLocations((l) => (l[p.id] === path ? l : { ...l, [p.id]: path }));
                        }}
                      />
                        </div>
                      </Fragment>
                    ))}
                  </div>
                ))}
                <div ref={setPreviewSlot} className="contents" />
              </div>
              <TerminalPanel
                ref={terminal}
                visible={terminalShown}
                height={terminalHeight}
                onHeight={setTerminalHeight}
                folder={terminalFolder}
                home={home}
                accent={accent}
                theme={theme}
                onHide={() => setTerminalShown(false)}
                maximized={maxed}
                onMaximize={() => setTerminalMax((m) => !m)}
                onPath={(path, isDir) => {
                  // A path followed from the output shows its folder: the folders come back.
                  setTerminalMax(false);
                  if (isDir) goTo(path);
                  else goTo(parentOf(path) ?? "/", path);
                }}
                onError={(text) => toast("error", text)}
              />
            </main>

            {menu && <ContextMenu x={menu.x} y={menu.y} items={menu.items} onClose={() => setMenu(null)} />}
            {ops.dialogs}
            {modal === "settings" && (
              <SettingsDialog
                accent={accent}
                onAccent={setAccent}
                appearance={appearance}
                onAppearance={setAppearance}
                systemLocales={boot.locales}
                prefs={{ statusBar, preview: showPreview, sidebar: !collapsed, terminal: terminalShown, photoMap, folderPreviews, folderPreviewsDrive }}
                onPref={(key: keyof WindowPrefs, value: boolean) => {
                  if (key === "photoMap") setPhotoMap(value);
                  else if (key === "folderPreviews") setFolderPreviews(value);
                  else if (key === "folderPreviewsDrive") setFolderPreviewsDrive(value);
                  else if (key === "statusBar") setStatusBar(value);
                  else if (key === "preview") setShowPreview(value);
                  else if (key === "sidebar") setCollapsed(!value);
                  else setTerminalShown(value);
                }}
                onClose={() => setModal(null)}
              />
            )}
            {modal === "about" && <AboutDialog boot={boot} onClose={() => setModal(null)} />}
            {propsFor && (
              <PropertiesDialog
                key={propsFor.join("\n")}
                paths={propsFor}
                boot={boot}
                showMap={photoMap}
                onPermissions={ops.setMode}
                onMeta={ops.writeMeta}
                onClose={() => setPropsFor(null)}
              />
            )}
            {details && <TaskDetails tasks={ops.tasks.filter((t) => details.includes(t.id))} home={home} onCancel={ops.cancelTask} onClose={closeDetails} />}
            {tipTab && tabTip && (
              <TabTip anchor={tabTip.anchor} rows={tipTab.panes.map((p) => ({ label: labelOf(locationOf(tipTab, p.id)), path: locationOf(tipTab, p.id) }))} />
            )}
          </div>
        </DndProvider>
      </RenameProvider>
    </CutProvider>
  );
}

/** A tab's folder (both, when split) with its full path, under the tab. */
function TabTip({ anchor, rows }: { anchor: DOMRect; rows: { label: string; path: string }[] }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [left, setLeft] = useState<number | null>(null);
  // Centred under the tab, inside the window.
  useLayoutEffect(() => {
    const w = ref.current?.offsetWidth ?? 0;
    setLeft(Math.min(Math.max(12, anchor.left + anchor.width / 2 - w / 2), window.innerWidth - 12 - w));
  }, [anchor]);
  return createPortal(
    <div
      ref={ref}
      role="tooltip"
      className="float-panel pointer-events-none fixed z-50 max-w-[28rem] space-y-1.5 px-3.5 py-2.5"
      style={{ left: left ?? 0, top: anchor.bottom + 6, visibility: left == null ? "hidden" : "visible" }}
    >
      {rows.map((r, i) => (
        <div key={i} className="min-w-0">
          <div className="flex items-center gap-2">
            <Folder className="h-3.5 w-3.5 flex-none accent-fg" />
            <span className="truncate text-[0.76rem] font-semibold text-ink">{r.label}</span>
          </div>
          <p className="mt-0.5 break-all text-[0.7rem] leading-[1.05rem] text-muted">{r.path === COMPUTER ? tr("window.computerTip") : r.path}</p>
        </div>
      ))}
    </div>,
    document.body,
  );
}

/**
 * The divider after pane `index` of a split of `panes`: dragging it moves
 * the border between that pane and the next (each keeps at least
 * `MIN_PANE`), a double-click evens all the panes out. A grip in the middle
 * shows it is there; it lights up in the accent under the pointer and while
 * dragged.
 */
function SplitDivider({ index, panes, onMove }: { index: number; panes: number; onMove: (x: number | null) => void }) {
  const [dragging, setDragging] = useState(false);
  const press = (ev: React.MouseEvent<HTMLDivElement>) => {
    if (ev.button !== 0) return;
    ev.preventDefault();
    const box = ev.currentTarget.parentElement!.getBoundingClientRect();
    // The panes' own width, without the dividers; the pointer is the middle of this one.
    const inner = box.width - (panes - 1) * DIVIDER;
    setDragging(true);
    document.body.classList.add("resizing-col");
    const move = (m: MouseEvent) => onMove((m.clientX - box.left - index * DIVIDER - DIVIDER / 2) / inner);
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      document.body.classList.remove("resizing-col");
      setDragging(false);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={tr("window.resizeSplit")}
      title={tr("window.resizeSplitTip")}
      onMouseDown={press}
      onDoubleClick={() => onMove(null)}
      className="group relative w-4 flex-none cursor-col-resize"
    >
      <span
        className={`absolute left-1/2 top-1/2 h-10 w-1 -translate-x-1/2 -translate-y-1/2 rounded-full transition-colors ${
          dragging ? "" : "bg-white/15 group-hover:bg-white/30"
        }`}
        style={dragging ? { background: "var(--accent)" } : undefined}
      />
    </div>
  );
}

