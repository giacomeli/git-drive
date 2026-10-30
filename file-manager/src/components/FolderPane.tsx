import {
  AppWindow,
  ClipboardPaste,
  Columns2,
  Copy,
  CopyPlus,
  ExternalLink,
  FileArchive,
  Folder,
  FolderOpen,
  FolderPlus,
  Info,
  Link,
  PackageOpen,
  PanelTop,
  PencilLine,
  Scissors,
  SlidersHorizontal,
  SquareTerminal,
  Trash2,
  Undo2,
  Wallpaper,
  X,
} from "lucide-react";
import { forwardRef, useCallback, useEffect, useImperativeHandle, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { api, baseName, errorText, parentOf, type AppChoice, type Boot, type ComputerData, type Entry, type Place } from "../lib/api";
import { copyText } from "../lib/clipboard";
import type { Dnd, DragPaths } from "../lib/dnd";
import type { FileOps } from "../lib/fileops";
import { archiveStem, isWallpaper } from "../lib/kinds";
import type { MarqueeHandlers } from "../lib/marquee";
import {
  COMPUTER,
  columnsFor,
  crumbsFor,
  go,
  goBack,
  goForward,
  goUp,
  gridMove,
  isWithin,
  moveTo,
  only,
  rebaseNav,
  startNav,
  type Nav,
} from "../lib/nav";
import { groupEntries, sortEntries, sortLabel, type GroupKey, type Sort, type SortKey } from "../lib/organize";
import { t, useI18n } from "../i18n";
import { marquee, rangeTo, selectAll, toggle, without } from "../lib/selection";
import { TERMINAL_KEYS } from "../lib/shortcuts";
import { useStored } from "../lib/stored";
import { useDirs, type DirState } from "../lib/useDir";
import { ColumnView } from "../views/ColumnView";
import { ComputerView } from "../views/ComputerView";
import { ICON_SIZE, IconSizeSlider, IconView } from "../views/IconView";
import { ListView } from "../views/ListView";
import { MultiPreview, PreviewPane, QuickLook } from "../views/Preview";
import { TrashBar, TrashLocked } from "../views/TrashView";
import { isMac } from "./Chrome";
import { ContextMenu, type Keys, type MenuItem } from "./ContextMenu";
import { Toolbar, type View } from "./Toolbar";

/** Move to Trash: Cmd+Backspace on macOS, Delete on Linux (as Finder and Files). */
const TRASH_KEYS: Keys = isMac ? { mod: true, key: "Backspace" } : { key: "Delete" };
/** Rename: Return on macOS (Finder; Cmd+O and Cmd+Down open), F2 on Linux (Files). */
const RENAME_KEYS: Keys = isMac ? { key: "Enter" } : { key: "F2" };

const GROUP_KEYS: GroupKey[] = ["none", "name", "kind", "modified", "created", "size"];
const SORT_KEYS: SortKey[] = ["name", "kind", "modified", "created", "size"];
/** A new sort key starts the way Finder starts it: names A to Z, dates and sizes newest and largest first. */
const STARTS_ASC: Record<SortKey, boolean> = { name: true, kind: true, modified: false, created: false, size: false };

/** What the window asks of a pane. */
export interface PaneHandle {
  navigate: (path: string, selected?: string | null) => void;
  /** Items pasted, created or dropped: selected where the pane shows them. */
  landed: (paths: string[]) => void;
  /** Items gone: out of the selection and the search results. */
  removed: (paths: string[]) => void;
  /** `from` is now `to`: the location, the selection and the history follow. */
  renamed: (from: string, to: string) => void;
  /** Reads the folders in view (and the search) again. */
  refresh: () => void;
  /** Whether `dir` is in view (the folder, or one of its columns). */
  shows: (dir: string) => boolean;
  showsHidden: () => boolean;
}

/** What the window gives its panes: its shared state and actions. */
export interface PaneEnv {
  boot: Boot;
  toast: (tone: "ok" | "error", text: string) => void;
  anchors: Place[];
  trashDir: string | null;
  ops: FileOps;
  dnd: Dnd;
  /** Why an item stays where it is, or null when it can move. */
  fixedReason: (entry: Entry) => string | null;
  bookmarkItem: (path: string) => MenuItem;
  openTerminalAt: (path: string) => void;
  terminalShown: boolean;
  toggleTerminal: () => void;
  showPreview: boolean;
  togglePreview: () => void;
  toggleSplit: () => void;
  newTab: () => void;
  openInNewTab: (path: string) => void;
  /** Shows `path` in a new pane right after this one (up to four; refused with a message when there is no room). */
  openInSplit: (path: string) => void;
  openSettings: () => void;
  openAbout: () => void;
  /** The Properties of these items (Finder's Get Info). */
  openProperties: (paths: string[]) => void;
  /** The status bar under the pane (Settings, and the window menu). */
  showStatusBar: boolean;
  toggleStatusBar: () => void;
  appBundle: string | null;
  sidebarCollapsed: boolean;
  /** Where the pane with the keys draws the toolbar and the preview. */
  toolbarSlot: HTMLElement | null;
  previewSlot: HTMLElement | null;
}

/** The folders in `dir` (hidden ones too: the field offers them once a "." is typed). */
const foldersIn = (dir: string) => api.listDir(dir, true).then((l) => l.entries.filter((e) => e.is_dir).map((e) => e.name));

/**
 * A folder pane: one folder in view (icons, list or columns), its history,
 * selection, search, menus and keys, as Finder's window shows one. A tab
 * holds one, or two side by side when split; the window gives the keys,
 * the toolbar and the preview to the pane in front (`active`).
 */
export const FolderPane = forwardRef<
  PaneHandle,
  {
    start: string;
    env: PaneEnv;
    /** The pane with the keys, the toolbar and the preview: in the tab in front, with no Settings over it. */
    active: boolean;
    /** A twin is beside it: the pane shows its folder on top, and whether it has the keys. */
    split: boolean;
    focused: boolean;
    onFocus: () => void;
    /** Closes this pane of a split. */
    onClose: () => void;
    /** Where the pane is (the sidebar's highlight, the tab's title, the terminal's folder). */
    onLocation: (path: string) => void;
  }
>(function FolderPane({ start, env, active, split, focused, onFocus, onClose, onLocation }, ref) {
  const { boot, toast, anchors, trashDir, ops, dnd } = env;
  const home = boot.places.find((p) => p.icon === "home")?.path ?? "/";
  const [nav, setNav] = useState<Nav>(() => startNav(start));
  const [view, setView] = useStored<View>("view", "list");
  const [showHidden, setShowHidden] = useStored("hidden", false);
  const [iconSize, setIconSize] = useStored("iconSize", ICON_SIZE.initial);
  // Icons per row of the icon view: how far Up and Down move.
  const gridColumns = useRef(1);
  const onGridColumns = useCallback((n: number) => {
    gridColumns.current = n;
  }, []);
  const [sort, setSort] = useStored<Sort>("sort", { key: "name", asc: true });
  const [group, setGroup] = useStored<GroupKey>("group", "none");
  const [computer, setComputer] = useState<ComputerData | null>(null);
  // A toolbar menu open under its button.
  const [dropdown, setDropdown] = useState<{ kind: "organize" | "more"; anchor: HTMLElement } | null>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<{ entries: Entry[]; truncated: boolean } | null>(null);
  const [searching, setSearching] = useState(false);
  // Bumped to run the search again after a change on disk.
  const [searchRun, setSearchRun] = useState(0);
  // A context menu: its items, or the item (or the empty space) it is for,
  // built at each render so a late answer (Open With's apps) shows in it.
  const [menu, setMenu] = useState<{ x: number; y: number; alt?: boolean; items?: MenuItem[]; entry?: Entry | null; dir?: string } | null>(
    null,
  );
  // Open With's apps for the file a menu opened on (null while they load).
  const [openWith, setOpenWith] = useState<{ path: string; apps: AppChoice[] | null } | null>(null);
  const [quickLook, setQuickLook] = useState(false);
  // The path as a field to type in: the text it opened with, or null (the breadcrumbs).
  const [typedPath, setTypedPath] = useState<string | null>(null);
  // The field belongs to the toolbar, which goes with the keys to the other pane.
  useEffect(() => {
    if (!active) setTypedPath(null);
  }, [active]);
  const searchRef = useRef<HTMLInputElement | null>(null);

  // The Computer view lists no folder; the Trash is a folder with its own rules.
  const virtual = nav.location === COMPUTER;
  const inTrash = trashDir != null && isWithin(nav.location, trashDir);
  const canEditHere = !virtual && !inTrash;

  const searchMode = query.trim() !== "";
  const columns = useMemo(
    () => (view === "columns" ? columnsFor(nav.location, anchors) : virtual ? [] : [nav.location]),
    [view, nav.location, anchors, virtual],
  );
  const { dirs, refresh } = useDirs(columns, showHidden);
  // The columns in the chosen order (they never group).
  const viewDirs = useMemo(() => {
    const out: Record<string, DirState> = {};
    for (const [path, state] of Object.entries(dirs))
      out[path] = state.listing ? { ...state, listing: { ...state.listing, entries: sortEntries(state.listing.entries, sort) } } : state;
    return out;
  }, [dirs, sort]);

  // Group names and the order by kind are in the app's language: a new one regroups.
  const { language: lang } = useI18n();
  // Entries the arrow keys move through, in the order the views show them (by section).
  const listGroups = useMemo(
    () => groupEntries(sortEntries(dirs[nav.location]?.listing?.entries ?? [], sort), group, sort),
    [dirs, nav.location, sort, group, lang],
  );
  const foundGroups = useMemo(
    () => groupEntries(sortEntries(results?.entries ?? [], sort), group, sort),
    [results, sort, group, lang],
  );
  const listEntries = useMemo(() => listGroups.flatMap((g) => g.entries), [listGroups]);
  const foundEntries = useMemo(() => foundGroups.flatMap((g) => g.entries), [foundGroups]);
  const shownGroups = searchMode ? foundGroups : listGroups;
  const activeDir =
    view === "columns" && nav.selected === nav.location && columns.length > 1 ? (parentOf(nav.location) ?? nav.location) : nav.location;
  const activeEntries: Entry[] = searchMode ? foundEntries : view === "columns" ? (viewDirs[activeDir]?.listing?.entries ?? []) : listEntries;

  /** The entry at `path`, among the search results or the folders read. */
  const entryAt = useCallback(
    (path: string): Entry | null => {
      if (searchMode) return results?.entries.find((e) => e.path === path) ?? null;
      const parent = parentOf(path);
      return (parent && dirs[parent]?.listing?.entries.find((e) => e.path === path)) || null;
    },
    [searchMode, results, dirs],
  );
  /** The focused item: the preview, Quick Look and the arrows follow it. */
  const selectedEntry: Entry | null = useMemo(() => (nav.selected ? entryAt(nav.selected) : null), [nav.selected, entryAt]);
  /** Every selected item; actions (copy, delete, drag) take them all. */
  const selectedEntries: Entry[] = useMemo(() => nav.selection.flatMap((p) => entryAt(p) ?? []), [nav.selection, entryAt]);
  const selectionSet = useMemo(() => new Set(nav.selection), [nav.selection]);
  const columnsMode = view === "columns" && !searchMode;
  /** The items of a view in the order it shows them: a column's, or the list's (the search results'). */
  const orderOf = (dir: string): string[] =>
    (columnsMode ? (viewDirs[dir]?.listing?.entries ?? []) : searchMode ? foundEntries : listEntries).map((e) => e.path);
  // A click on one of several selected items narrows the selection at the
  // release, unless a drag started: Finder keeps the selection for the drag.
  const narrowOnRelease = useRef<(() => void) | null>(null);
  const isFixed = (entry: Entry) => env.fixedReason(entry) != null;

  // The window learns where the pane is.
  useEffect(() => {
    onLocation(nav.location);
    // Only the location: the callback is the window's and new each render.
  }, [nav.location]);

  // --- navigation actions ---
  const navigate = useCallback((path: string, selected: string | null = null) => {
    setQuery("");
    setNav((n) => go(n, path, selected));
  }, []);

  /** One item selected, as a click selects it; in column view a folder opens as the last column. */
  const selectIn = useCallback(
    (n: Nav, entry: Entry, columnDir?: string): Nav => {
      if (view !== "columns" || searchMode) return { ...n, ...only(entry.path) };
      return entry.is_dir ? moveTo(n, entry.path, entry.path) : moveTo(n, columnDir ?? n.location, entry.path);
    },
    [view, searchMode],
  );
  const select = useCallback((entry: Entry, columnDir?: string) => setNav((n) => selectIn(n, entry, columnDir)), [selectIn]);

  /** Column view: a selection lives in one column; a click in another starts over there. */
  const within = (n: Nav, dir: string): Nav => {
    if (!columnsMode) return n;
    return n.selection.length > 0 && n.selection.every((p) => parentOf(p) === dir) ? n : { ...n, ...only(null) };
  };
  /** Column view: one folder selected opens as the last column; anything else leaves its own column last. */
  const settle = (n: Nav, dir: string): Nav => {
    if (!columnsMode) return n;
    const single = n.selection.length === 1 ? entryAt(n.selection[0]) : null;
    return single?.is_dir ? { ...n, location: single.path, selected: single.path } : { ...n, location: dir };
  };

  const open = useCallback(
    (entry: Entry) => {
      if (entry.is_dir) {
        navigate(entry.path, view === "columns" ? entry.path : null);
      } else {
        api.openPath(entry.path).catch((e) => toast("error", errorText(e)));
      }
    },
    [navigate, view, toast],
  );
  /** Several items: each file opens in its app; a folder opens only alone (in this pane). */
  const openItems = (items: Entry[]) => {
    if (items.length === 1) return open(items[0]);
    for (const it of items) if (!it.is_dir) api.openPath(it.path).catch((e) => toast("error", errorText(e)));
  };
  /** A double-click: with Cmd (Ctrl) on a folder, a new tab, as in Finder; on one of several selected items, all of them. */
  const openFrom = (entry: Entry, ev?: React.MouseEvent) => {
    if (ev && entry.is_dir && (isMac ? ev.metaKey : ev.ctrlKey)) return env.openInNewTab(entry.path);
    return selectionSet.has(entry.path) && selectedEntries.length > 1 ? openItems(selectedEntries) : open(entry);
  };

  /** Something changed on disk: re-read the folders in view and the search. */
  const changed = () => {
    refresh();
    if (searchMode) setSearchRun((n) => n + 1);
  };

  useImperativeHandle(ref, () => ({
    navigate,
    landed: (paths) => {
      const first = paths[0];
      const parent = first ? parentOf(first) : null;
      if (!first || !parent) return;
      // Column view: selected in their column, a folder not opened.
      setNav((n) => {
        const one = view === "columns" && !searchMode ? moveTo(n, parent, first) : { ...n, ...only(first) };
        return paths.length > 1 ? { ...one, selection: paths, selected: paths[paths.length - 1], anchor: first } : one;
      });
    },
    removed: (paths) => {
      setNav((n) => {
        const left = without(n, paths);
        // Column view, nothing left selected: the folder of the last column is.
        return view === "columns" && left !== n && left.selection.length === 0 ? { ...left, ...only(left.location) } : left;
      });
      setResults((r) => (r ? { ...r, entries: r.entries.filter((e) => !paths.includes(e.path)) } : r));
    },
    renamed: (from, to) => setNav((n) => rebaseNav(n, from, to)),
    refresh: changed,
    shows: (dir) => nav.location === dir || columns.includes(dir),
    showsHidden: () => showHidden,
  }));

  /** What a drag from `entry` carries: the selection it is in, or the reason it stays. */
  const dragPaths = (entry: Entry): DragPaths => {
    const items = selectionSet.has(entry.path) ? selectedEntries : [entry];
    if (items.some((it) => ops.renaming?.path === it.path)) return null;
    const refused = items.map(env.fixedReason).find((r) => r != null);
    return refused ? { refused } : items.map((it) => it.path);
  };
  // Where Paste and New Folder go from the keyboard: beside the selection,
  // else the folder in view.
  const keyDir = (selectedEntry && parentOf(selectedEntry.path)) || nav.location;

  /** A press on an item: Finder's clicks (Cmd, or Ctrl on Linux, adds or takes out; Shift extends a range
   * in a list or a column and adds in the icon view), then a possible drag. The middle button opens a
   * folder in a new tab. */
  const pressItem = (ev: React.MouseEvent, entry: Entry, columnDir?: string) => {
    if (ev.button === 1 && entry.is_dir) {
      ev.preventDefault();
      env.openInNewTab(entry.path);
      return;
    }
    const inSelection = selectionSet.has(entry.path);
    // The menu's button (or Ctrl+click on macOS) acts on the selection the item is in, else on the item alone.
    if (ev.button !== 0 || (isMac && ev.ctrlKey)) {
      if (!inSelection) select(entry, columnDir);
      return;
    }
    const adding = isMac ? ev.metaKey : ev.ctrlKey;
    if (adding || ev.shiftKey) {
      const dir = columnDir ?? parentOf(entry.path) ?? nav.location;
      const range = ev.shiftKey && !adding && view !== "icons";
      setNav((n) => settle(range ? rangeTo(within(n, dir), entry.path, orderOf(dir)) : toggle(within(n, dir), entry.path), dir));
      return;
    }
    if (inSelection && nav.selection.length > 1) {
      narrowOnRelease.current = () => select(entry, columnDir);
      const release = () => {
        window.removeEventListener("mouseup", release);
        narrowOnRelease.current?.();
        narrowOnRelease.current = null;
      };
      window.addEventListener("mouseup", release);
    } else {
      select(entry, columnDir);
    }
    dnd.press(
      ev,
      entry,
      () => dragPaths(entry),
      () => {
        narrowOnRelease.current = null;
      },
    );
  };

  /** The selection rectangle of the icon and list views. */
  const rectangle: MarqueeHandlers = {
    onStart: () => nav.selection,
    onChange: (hits, additive, base) => setNav((n) => marquee(n, base, hits, additive, orderOf(n.location))),
  };

  const openMenu = (ev: React.MouseEvent, entry: Entry | null, dir?: string) => {
    const where = (entry && parentOf(entry.path)) || dir || nav.location;
    setMenu({ x: ev.clientX, y: ev.clientY, alt: ev.altKey, entry, dir: where });
    // Open With's apps load as the menu opens (icons take a moment the first time).
    if (entry && !entry.is_dir && !inTrash && openWith?.path !== entry.path) {
      const path = entry.path;
      setOpenWith({ path, apps: null });
      api.openWithApps(path).then(
        (apps) => setOpenWith((cur) => (cur?.path === path ? { path, apps } : cur)),
        () => setOpenWith((cur) => (cur?.path === path ? { path, apps: [] } : cur)),
      );
    }
  };

  const copyPaths = (paths: string[]) =>
    copyText(paths.join("\n")).then(
      () => toast("ok", paths.length === 1 ? t("files.pathCopied") : t("files.pathsCopied", { count: paths.length })),
      (e) => toast("error", errorText(e)),
    );

  const openWithApp = (paths: string[], app: AppChoice, always: boolean) =>
    api.openWith(paths, app.id, always).then(
      () => {
        const ext = baseName(paths[0]).includes(".") ? baseName(paths[0]).split(".").pop() : null;
        if (always) toast("ok", ext ? t("files.alwaysOpenExt", { ext, app: app.name }) : t("files.alwaysOpenAny", { app: app.name }));
      },
      (e) => toast("error", errorText(e)),
    );

  /** Open With's submenu: the default app, then the others; with `always`, the choice becomes the default. */
  const appItems = (paths: string[], always: boolean): MenuItem[] | "loading" => {
    const apps = openWith?.path === paths[0] ? openWith.apps : null;
    if (apps == null) return "loading";
    if (apps.length === 0) return [{ label: t("menu.noApps"), disabled: true }];
    const row = (a: AppChoice): MenuItem => ({
      label: a.default ? t("menu.appDefault", { name: a.name }) : a.name,
      image: a.icon,
      Icon: AppWindow,
      run: () => void openWithApp(paths, a, always),
    });
    const [first, ...rest] = apps;
    return first.default && rest.length > 0 ? [row(first), "separator", ...rest.map(row)] : apps.map(row);
  };

  const setDesktopPicture = (path: string) =>
    api.setDesktopPicture(path).then(
      () => toast("ok", t("files.desktopPicture", { name: baseName(path) })),
      (e) => toast("error", errorText(e)),
    );

  /** Inside the Trash: put back (Linux) or delete for good; the empty space empties it. */
  const trashMenuItems = (entry: Entry | null): MenuItem[] => {
    const count = dirs[nav.location]?.listing?.entries.length ?? 0;
    if (!entry) return [{ label: t("menu.emptyTrash"), Icon: Trash2, danger: true, disabled: count === 0, run: () => ops.emptyTrash(count) }];
    const targets = selectionSet.has(entry.path) && selectedEntries.length > 1 ? selectedEntries : [entry];
    const many = targets.length > 1;
    const n = { count: targets.length };
    const paths = targets.map((x) => x.path);
    // Only the top level has a record of where it came from.
    const topLevel = paths.every((p) => parentOf(p) === trashDir);
    const items: MenuItem[] = [];
    if (boot.platform === "Linux")
      items.push({ label: many ? t("menu.putBackMany", n) : t("menu.putBack"), Icon: Undo2, disabled: !topLevel, run: () => ops.putBack(paths) });
    items.push(
      { label: many ? t("menu.deleteNowMany", n) : t("menu.deleteNow"), Icon: Trash2, keys: TRASH_KEYS, danger: true, run: () => ops.deleteForGood(paths) },
      "separator",
      { label: many ? t("menu.copyMany", n) : t("menu.copy"), Icon: Copy, keys: { mod: true, key: "C" }, run: () => ops.copy(targets) },
    );
    return items;
  };

  /** Linux: items of the drive have no Trash to go to (a delete there is for good). */
  const noTrashFor = (paths: string[]) => {
    const drive = boot.places.find((p) => p.icon === "drive")?.path;
    return boot.platform === "Linux" && drive != null && paths.some((p) => p.startsWith(`${drive}/`));
  };

  const menuItems = (entry: Entry | null, dir: string): MenuItem[] => {
    const clip = ops.clip;
    const clipName = clip ? (clip.paths.length === 1 ? t("menu.clipOne", { name: baseName(clip.paths[0]) }) : t("menu.clipMany", { count: clip.paths.length })) : "";
    const paste: MenuItem = {
      label: !clip ? t("menu.paste") : clip.cut ? t("menu.moveHere", { name: clipName }) : t("menu.pasteNamed", { name: clipName }),
      Icon: ClipboardPaste,
      keys: { mod: true, key: "V" },
      disabled: !clip,
      run: () => ops.paste(dir),
    };
    const newFolder: MenuItem = {
      label: t("menu.newFolder"),
      Icon: FolderPlus,
      keys: { mod: true, shift: true, key: "N" },
      run: () => ops.newFolder(dir),
    };
    const copyPath = (paths: string[]): MenuItem => ({
      label: paths.length > 1 ? t("menu.copyPaths", { count: paths.length }) : t("menu.copyPath"),
      Icon: Link,
      keys: { mod: true, alt: true, key: "C" },
      when: "alt",
      run: () => void copyPaths(paths),
    });
    const openInTerminal = (path: string): MenuItem => ({ label: t("menu.openInTerminal"), Icon: SquareTerminal, run: () => env.openTerminalAt(path) });
    const properties = (paths: string[]): MenuItem => ({ label: t("menu.properties"), Icon: Info, keys: { mod: true, key: "I" }, run: () => env.openProperties(paths) });
    if (!entry) return [newFolder, "separator", paste, "separator", env.bookmarkItem(dir), openInTerminal(dir), copyPath([dir]), "separator", properties([dir])];
    // On one of several selected items, the menu acts on them all.
    const targets = selectionSet.has(entry.path) && selectedEntries.length > 1 ? selectedEntries : [entry];
    const many = targets.length > 1;
    const n = { count: targets.length };
    const paths = targets.map((x) => x.path);
    // Linux: the drive has no Trash, so this asks before deleting for good.
    const noTrash = noTrashFor(paths);
    const fixed = targets.some(isFixed);
    const folderAlone = targets.length === 1 && entry.is_dir;
    const single = targets.length === 1 ? entry : null;
    // What only this kind of file offers: Open With for a file; a picture, an archive.
    const openWithItems: MenuItem[] =
      single && !single.is_dir
        ? [
            { label: t("menu.openWith"), Icon: AppWindow, when: "plain", submenu: appItems(paths, false) },
            { label: t("menu.alwaysOpenWith"), Icon: AppWindow, when: "alt", submenu: appItems(paths, true) },
          ]
        : [];
    const kindItems: MenuItem[] = [
      ...(single && isWallpaper(single)
        ? [{ label: t("menu.setDesktopPicture"), Icon: Wallpaper, run: () => void setDesktopPicture(single.path) } satisfies MenuItem]
        : []),
      ...(boot.archives && single && !single.is_dir && archiveStem(single.name)
        ? [{ label: t("menu.extractHere"), Icon: PackageOpen, run: () => ops.extract(single.path) } satisfies MenuItem]
        : []),
    ];
    // New Folder and Compress lead: the two most asked for on an item.
    return [
      newFolder,
      ...(boot.archives
        ? [
            {
              label: many ? t("menu.compressMany", n) : t("menu.compressOne", { name: entry.name }),
              Icon: FileArchive,
              run: () => ops.compress(paths),
            } satisfies MenuItem,
          ]
        : []),
      "separator",
      {
        label: many ? t("menu.openMany", n) : t("menu.open"),
        Icon: entry.is_dir && !many ? FolderOpen : ExternalLink,
        keys: isMac ? { mod: true, key: "O" } : { key: "Enter" },
        run: () => openItems(targets),
      },
      ...(folderAlone
        ? [
            { label: t("menu.openInNewTab"), Icon: PanelTop, run: () => env.openInNewTab(entry.path) } satisfies MenuItem,
            { label: t("menu.openInSplit"), Icon: Columns2, run: () => env.openInSplit(entry.path) } satisfies MenuItem,
          ]
        : []),
      ...openWithItems,
      "separator",
      ...kindItems,
      "separator",
      { label: many ? t("menu.cutMany", n) : t("menu.cut"), Icon: Scissors, keys: { mod: true, key: "X" }, run: () => ops.cut(targets) },
      { label: many ? t("menu.copyMany", n) : t("menu.copy"), Icon: Copy, keys: { mod: true, key: "C" }, run: () => ops.copy(targets) },
      copyPath(paths),
      paste,
      {
        label: many ? t("menu.duplicateMany", n) : t("menu.duplicate"),
        Icon: CopyPlus,
        keys: { mod: true, key: "D" },
        when: "alt",
        run: () => ops.duplicate(paths),
      },
      "separator",
      {
        label: t("menu.rename"),
        Icon: PencilLine,
        keys: RENAME_KEYS,
        disabled: many || fixed,
        run: () => ops.rename(entry),
      },
      {
        label: noTrash ? t("menu.deleteNow") : t("menu.moveToTrash"),
        Icon: Trash2,
        keys: TRASH_KEYS,
        danger: true,
        disabled: fixed,
        run: () => ops.trash(paths, noTrash),
      },
      ...(folderAlone ? ["separator" as const, env.bookmarkItem(entry.path), openInTerminal(entry.path)] : []),
      "separator",
      properties(paths),
    ];
  };

  // --- toolbar menus ---
  const organizeItems = (): MenuItem[] => {
    // The column view sorts but never groups.
    const noGroups = view === "columns" && !searchMode;
    return [
      { heading: noGroups ? t("menu.groupByNoColumns") : t("menu.groupBy") },
      ...GROUP_KEYS.map(
        (k): MenuItem => ({ label: k === "none" ? t("menu.none") : sortLabel(k), checked: group === k, disabled: noGroups, run: () => setGroup(k) }),
      ),
      "separator",
      { heading: t("menu.sortBy") },
      ...SORT_KEYS.map((k): MenuItem => ({ label: sortLabel(k), checked: sort.key === k, run: () => setSort({ key: k, asc: STARTS_ASC[k] }) })),
      "separator",
      { label: t("menu.ascending"), checked: sort.asc, run: () => setSort((s) => ({ ...s, asc: true })) },
      { label: t("menu.descending"), checked: !sort.asc, run: () => setSort((s) => ({ ...s, asc: false })) },
    ];
  };
  const moreItems = (): MenuItem[] => [
    { label: t("menu.newTab"), Icon: PanelTop, keys: { mod: true, key: "T" }, run: env.newTab },
    {
      label: t("menu.newFolder"),
      Icon: FolderPlus,
      keys: { mod: true, shift: true, key: "N" },
      disabled: !canEditHere,
      run: () => ops.newFolder(keyDir),
    },
    "separator",
    { label: t("menu.splitView"), checked: split, keys: { mod: true, key: "/" }, run: env.toggleSplit },
    { label: t("menu.showHidden"), checked: showHidden, keys: { mod: true, shift: true, key: "." }, run: () => setShowHidden((h) => !h) },
    { label: t("menu.showTerminal"), checked: env.terminalShown, keys: { mod: true, key: "J" }, run: env.toggleTerminal },
    {
      label: t("menu.showPreview"),
      checked: env.showPreview,
      keys: { mod: true, shift: true, key: "P" },
      disabled: virtual,
      run: env.togglePreview,
    },
    { label: t("menu.showStatusBar"), checked: env.showStatusBar, run: env.toggleStatusBar },
    "separator",
    { label: t("menu.settings"), Icon: SlidersHorizontal, keys: { mod: true, key: "," }, run: env.openSettings },
    { label: t("menu.about"), Icon: Info, run: env.openAbout },
  ];
  const toggleDropdown = (kind: "organize" | "more") => (anchor: HTMLElement) =>
    setDropdown((d) => (d?.kind === kind ? null : { kind, anchor }));

  const changeView = (v: View) => {
    setView(v);
    // Column view keeps the selected folder open in the last column.
    if (v === "columns" && selectedEntry?.is_dir && nav.selection.length === 1)
      setNav((n) => moveTo(n, selectedEntry.path, selectedEntry.path));
    if (v !== "columns") setNav((n) => (n.selected === n.location ? { ...n, ...only(null) } : n));
  };

  // --- search: debounced, results stream in batches; a newer search cancels
  // the running one in the backend and its late batches are ignored here ---
  useEffect(() => {
    if (!searchMode) {
      setResults(null);
      setSearching(false);
      return;
    }
    let alive = true;
    setSearching(true);
    const root = nav.location;
    const timer = window.setTimeout(() => {
      setResults({ entries: [], truncated: false });
      api
        .search(root, query, showHidden, (batch) => {
          if (alive) setResults((r) => ({ entries: [...(r?.entries ?? []), ...batch], truncated: r?.truncated ?? false }));
        })
        .then((summary) => {
          if (alive && !summary.cancelled) {
            setResults((r) => ({ entries: r?.entries ?? [], truncated: summary.truncated }));
            setSearching(false);
          }
        })
        .catch((e) => {
          if (alive) {
            setSearching(false);
            toast("error", errorText(e));
          }
        });
    }, 250);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [query, showHidden, nav.location, searchMode, toast, searchRun]);

  // --- the Computer view ---
  const loadComputer = useCallback(() => {
    api
      .computer()
      .then(setComputer)
      .catch((e) => toast("error", errorText(e)));
  }, [toast]);
  useEffect(() => {
    if (virtual) loadComputer();
  }, [virtual, loadComputer]);

  // Folders may change behind our back (the drive pulls): re-read on focus.
  useEffect(() => {
    const onWindowFocus = () => {
      refresh();
      if (virtual) loadComputer();
    };
    window.addEventListener("focus", onWindowFocus);
    return () => window.removeEventListener("focus", onWindowFocus);
  }, [refresh, loadComputer, virtual]);

  // --- keyboard, Finder-style: the pane with the keys only (the window takes
  // its own first: tabs, splits, the terminal, Settings) ---
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      // The menus and the dialogs own the keyboard while open.
      if (menu || dropdown || ops.dialogOpen || document.querySelector('[role="menu"], [role="dialog"]')) return;
      const target = e.target as HTMLElement | null;
      // In a terminal, every key is the shell's.
      if (target?.closest?.(".xterm")) return;
      const typing = target?.tagName === "INPUT" || target?.tagName === "TEXTAREA";
      const mod = e.metaKey || e.ctrlKey;
      if (e.key === "Escape") {
        if (quickLook) setQuickLook(false);
        else if (query) setQuery("");
        searchRef.current?.blur();
        return;
      }
      // The path as a field: Cmd+L (Ctrl+L), and Finder's Go to Folder (Shift+Cmd+G).
      if (mod && !e.altKey && ((e.code === "KeyL" && !e.shiftKey) || (isMac && e.shiftKey && e.code === "KeyG"))) {
        e.preventDefault();
        setTypedPath(virtual ? "" : nav.location);
        return;
      }
      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (typing && e.key !== "ArrowDown") return;
      if (typing && e.key === "ArrowDown") searchRef.current?.blur();
      // "/" or "~" typed into the view starts a path, as in Finder.
      if (!mod && !e.altKey && (e.key === "/" || e.key === "~")) return void (e.preventDefault(), setTypedPath(e.key));

      if (mod && e.key === "[") return void (e.preventDefault(), setQuery(""), setNav(goBack));
      if (mod && e.key === "]") return void (e.preventDefault(), setQuery(""), setNav(goForward));
      if (mod && e.key === "ArrowUp") return void (e.preventDefault(), setQuery(""), setNav(goUp));
      if (mod && e.shiftKey && e.code === "Period") return void (e.preventDefault(), setShowHidden((h) => !h));
      // The views: Cmd+Option+1 to 3 (Cmd+1 to 9 go to tabs). By key position: Option changes the character.
      if (mod && e.altKey && e.code === "Digit1") return void (e.preventDefault(), changeView("icons"));
      if (mod && e.altKey && e.code === "Digit2") return void (e.preventDefault(), changeView("list"));
      if (mod && e.altKey && e.code === "Digit3") return void (e.preventDefault(), changeView("columns"));
      if (mod && e.key.toLowerCase() === "r") return void (e.preventDefault(), refresh(), virtual && loadComputer());
      // The Computer view has no items to act on.
      if (virtual) return;
      // Copy Path (Option+Cmd+C, Ctrl+Alt+C): by the key's position, since Option changes its character on a Mac.
      if (mod && e.altKey && e.code === "KeyC") {
        e.preventDefault();
        void copyPaths(selectedEntries.length > 0 ? selectedEntries.map((x) => x.path) : [nav.location]);
        return;
      }
      // Properties (Cmd+I, Ctrl+I), as Finder's Get Info: the selection, else the folder in view.
      if (mod && !e.shiftKey && !e.altKey && e.code === "KeyI") {
        e.preventDefault();
        env.openProperties(selectedEntries.length > 0 ? selectedEntries.map((x) => x.path) : [nav.location]);
        return;
      }
      // Duplicate (Cmd+D, Ctrl+D), as Finder's.
      if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "d") {
        e.preventDefault();
        if (canEditHere && selectedEntries.length > 0) ops.duplicate(selectedEntries.map((x) => x.path));
        return;
      }
      // Copy and Cut leave selected text to the webview (the text preview).
      const textSelected = (window.getSelection()?.toString() ?? "") !== "";
      if (
        mod &&
        !e.shiftKey &&
        (e.key.toLowerCase() === "c" || e.key.toLowerCase() === "x") &&
        selectedEntries.length > 0 &&
        !textSelected
      ) {
        e.preventDefault();
        (e.key.toLowerCase() === "c" ? ops.copy : ops.cut)(selectedEntries);
        return;
      }
      if (mod && !e.shiftKey && e.key.toLowerCase() === "a" && !textSelected) {
        e.preventDefault();
        setNav((n) => settle(selectAll(within(n, activeDir), orderOf(activeDir)), activeDir));
        return;
      }
      if (mod && !e.shiftKey && e.key.toLowerCase() === "v") return void (e.preventDefault(), canEditHere && ops.paste(keyDir));
      if (mod && e.shiftKey && e.key.toLowerCase() === "n") return void (e.preventDefault(), canEditHere && ops.newFolder(keyDir));
      if (selectedEntries.length > 0 && (isMac ? mod && e.key === "Backspace" : e.key === "Delete")) {
        e.preventDefault();
        const paths = selectedEntries.map((s) => s.path);
        if (inTrash) ops.deleteForGood(paths);
        else if (!selectedEntries.some(isFixed)) ops.trash(paths, noTrashFor(paths));
        return;
      }
      if (view === "icons" && mod && (e.key === "=" || e.key === "+" || e.key === "-")) {
        e.preventDefault();
        const step = e.key === "-" ? -ICON_SIZE.step : ICON_SIZE.step;
        setIconSize((s) => Math.max(ICON_SIZE.min, Math.min(ICON_SIZE.max, s + step)));
        return;
      }
      if (!mod && (isMac ? e.key === "Enter" : e.key === "F2")) {
        e.preventDefault();
        if (selectedEntry && selectedEntries.length === 1 && !isFixed(selectedEntry) && !inTrash) ops.rename(selectedEntry);
        return;
      }
      if ((mod && (e.key === "ArrowDown" || e.key.toLowerCase() === "o")) || e.key === "Enter") {
        e.preventDefault();
        if (selectedEntries.length > 0) openItems(selectedEntries);
        return;
      }
      if (e.key === " ") {
        e.preventDefault();
        if (selectedEntry) setQuickLook((q) => !q);
        return;
      }
      if (view === "icons" && e.key.startsWith("Arrow")) {
        e.preventDefault();
        const order = activeEntries;
        const { key, shiftKey: extend } = e;
        // From the latest state: keys can arrive faster than a render.
        setNav((n) => {
          const next = gridMove(
            order.findIndex((x) => x.path === n.selected),
            shownGroups.map((g) => g.entries.length),
            gridColumns.current,
            key,
          );
          if (next == null) return n;
          // Shift extends the selection from its anchor, as Finder does.
          return extend ? rangeTo(n, order[next].path, orderOf(activeDir)) : selectIn(n, order[next]);
        });
        return;
      }
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const order = activeEntries;
        if (order.length === 0) return;
        const down = e.key === "ArrowDown";
        const extend = e.shiftKey;
        // From the latest state: keys can arrive faster than a render.
        setNav((n) => {
          const idx = order.findIndex((x) => x.path === n.selected);
          const next = idx < 0 ? (down ? 0 : order.length - 1) : Math.max(0, Math.min(order.length - 1, idx + (down ? 1 : -1)));
          return extend
            ? settle(rangeTo(within(n, activeDir), order[next].path, orderOf(activeDir)), activeDir)
            : selectIn(n, order[next], activeDir);
        });
        return;
      }
      if (view === "columns" && !searchMode && (e.key === "ArrowRight" || e.key === "ArrowLeft")) {
        e.preventDefault();
        if (e.key === "ArrowRight" && nav.selected === nav.location) {
          const first = viewDirs[nav.location]?.listing?.entries[0];
          if (first) select(first, nav.location);
        } else if (e.key === "ArrowLeft") {
          if (nav.selected && nav.selected !== nav.location) setNav((n) => moveTo(n, n.location, n.location));
          else if (columns.length > 1) {
            const parent = parentOf(nav.location);
            if (parent) setNav((n) => moveTo(n, parent, parent));
          }
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  /** Goes where a typed path leads (a file: its folder, with it selected); the reason when it can't. */
  const goTyped = async (typed: string): Promise<string | null> => {
    try {
      const to = await api.resolvePath(typed, virtual ? home : nav.location);
      setTypedPath(null);
      if (to.is_dir) navigate(to.path);
      else navigate(parentOf(to.path) ?? "/", to.path);
      return null;
    } catch (e) {
      return errorText(e);
    }
  };

  const crumbs = crumbsFor(nav.location, anchors).map((c) => (c.path === trashDir ? { ...c, label: t("files.trash") } : c));
  const count = searchMode ? (results?.entries.length ?? 0) : (dirs[nav.location]?.listing?.entries.length ?? 0);
  const sidePreview = active && env.showPreview && !virtual;
  // Column view: a folder open as the last column is already in view; the panel sums it up instead.
  const previewEntry = columnsMode && selectedEntry?.is_dir ? null : selectedEntry;
  const previewFolder = searchMode
    ? undefined
    : { name: baseName(nav.location), path: nav.location, items: dirs[nav.location]?.listing?.entries.length ?? 0 };
  const clearSelection = () => setNav((n) => ({ ...n, ...only(null) }));
  const here = dirs[nav.location];
  // macOS keeps ~/.Trash from every app but Finder (without Full Disk Access);
  // on Linux a Trash never used yet has no folder.
  const trashLocked = inTrash && boot.platform === "macOS" && here?.error != null;
  const trashMissing = inTrash && here?.error != null && /os error 2\b|No such file/.test(here.error);
  const folderEmpty = inTrash && nav.location === trashDir ? t("files.trashEmpty") : t("files.emptyFolder");
  const emptyText = trashMissing ? folderEmpty : (here?.error ?? (here?.loading ? "" : folderEmpty));
  const dropdownRect = dropdown?.anchor.getBoundingClientRect();
  const title = crumbs[crumbs.length - 1]?.label ?? baseName(nav.location);

  const toolbar = (
    <Toolbar
      inset={isMac && env.sidebarCollapsed}
      canBack={nav.back.length > 0}
      canForward={nav.forward.length > 0}
      onBack={() => {
        setQuery("");
        setNav(goBack);
      }}
      onForward={() => {
        setQuery("");
        setNav(goForward);
      }}
      crumbs={crumbs}
      onCrumb={(p) => navigate(p)}
      editing={typedPath}
      onEdit={(open) => setTypedPath(open ? (virtual ? "" : nav.location) : null)}
      onGo={goTyped}
      folders={foldersIn}
      home={home}
      view={view}
      onView={changeView}
      onOrganize={toggleDropdown("organize")}
      organizeOn={group !== "none" || sort.key !== "name" || !sort.asc}
      onMore={toggleDropdown("more")}
      folderView={!virtual}
      query={query}
      onQuery={(q) => {
        setQuery(q);
        setNav((n) => ({ ...n, ...only(null) }));
      }}
      searching={searching}
      searchRef={searchRef}
    />
  );

  return (
    <>
      {active && env.toolbarSlot && createPortal(toolbar, env.toolbarSlot)}
      <section
        // In a split, the pane without the keys steps back.
        className={`glass flex min-w-0 flex-1 flex-col overflow-hidden ${split && !focused ? "pane-dim" : ""}`}
        // In a split, the pane with the keys says so with the accent; a press anywhere in a pane gives it the keys.
        style={split && focused ? { boxShadow: "inset 0 0 0 1px rgb(var(--a1-rgb) / 0.75)" } : undefined}
        onMouseDownCapture={onFocus}
        data-pane-focused={focused ? "" : undefined}
      >
        {split && (
          <div className="flex h-8 flex-none items-center gap-2 border-b border-line pl-4 pr-1.5 text-[0.74rem]">
            {virtual ? <Columns2 className="h-3.5 w-3.5 text-dim" /> : <Folder className={`h-3.5 w-3.5 ${focused ? "accent-fg" : "text-dim"}`} />}
            <span className={`min-w-0 flex-1 truncate ${focused ? "font-semibold text-ink" : "text-muted"}`} title={nav.location}>
              {title}
            </span>
            <button
              aria-label={t("files.closePane")}
              title={t("files.closePane")}
              onClick={(ev) => {
                ev.stopPropagation();
                onClose();
              }}
              className="grid h-6 w-6 place-items-center rounded-md text-dim transition-colors hover:bg-white/8 hover:text-ink"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        )}
        {inTrash && !trashLocked && <TrashBar count={trashMissing ? 0 : count} onEmpty={() => ops.emptyTrash(count)} />}
        {virtual ? (
          <ComputerView
            data={computer}
            onOpen={(p) => navigate(p)}
            onDirMenu={(ev, place) =>
              setMenu({
                x: ev.clientX,
                y: ev.clientY,
                items: [
                  { label: t("menu.open"), Icon: FolderOpen, run: () => navigate(place.path) },
                  { label: t("menu.openInNewTab"), Icon: PanelTop, run: () => env.openInNewTab(place.path) },
                  { label: t("menu.openInSplit"), Icon: Columns2, run: () => env.openInSplit(place.path) },
                  "separator",
                  env.bookmarkItem(place.path),
                ],
              })
            }
          />
        ) : trashLocked && !searchMode ? (
          <TrashLocked
            onAllow={() => api.openFullDiskAccess().catch((e) => toast("error", errorText(e)))}
            onOpenInFinder={() => api.openPath(trashDir!).catch((e) => toast("error", errorText(e)))}
            onShowApp={env.appBundle ? () => api.openPath(env.appBundle!, true).catch((e) => toast("error", errorText(e))) : undefined}
          />
        ) : view === "icons" ? (
          <IconView
            groups={shownGroups}
            iconSize={iconSize}
            selection={selectionSet}
            focus={nav.selected}
            onPress={(ev, e) => pressItem(ev, e)}
            onOpen={openFrom}
            onClear={clearSelection}
            marquee={rectangle}
            onColumns={onGridColumns}
            searchRoot={searchMode ? nav.location : undefined}
            dir={searchMode || inTrash ? undefined : nav.location}
            onContext={openMenu}
            empty={searchMode ? (searching ? t("files.searching") : t("files.nothingNamed", { query: query.trim(), folder: baseName(nav.location) })) : emptyText}
          />
        ) : searchMode ? (
          <ListView
            groups={foundGroups}
            sort={sort}
            onSort={setSort}
            selection={selectionSet}
            focus={nav.selected}
            onPress={(ev, e) => pressItem(ev, e)}
            onOpen={openFrom}
            onClear={clearSelection}
            marquee={rectangle}
            whereRoot={nav.location}
            onContext={openMenu}
            empty={searching ? t("files.searching") : t("files.nothingNamed", { query: query.trim(), folder: baseName(nav.location) })}
          />
        ) : view === "list" ? (
          <ListView
            groups={listGroups}
            sort={sort}
            onSort={setSort}
            selection={selectionSet}
            focus={nav.selected}
            onPress={(ev, e) => pressItem(ev, e)}
            onOpen={openFrom}
            onClear={clearSelection}
            marquee={rectangle}
            dir={inTrash ? undefined : nav.location}
            onContext={openMenu}
            empty={emptyText}
          />
        ) : (
          <ColumnView
            columns={columns}
            dirs={viewDirs}
            selection={selectionSet}
            focus={nav.selected}
            onPress={pressItem}
            onOpen={openFrom}
            onContext={openMenu}
            onBackground={(ev) => openMenu(ev, null, columns[columns.length - 1] ?? nav.location)}
          />
        )}
        {env.showStatusBar && (
          <footer className="flex h-8 flex-none items-center justify-between border-t border-line px-5 text-[0.68rem] text-dim">
            <span className="flex min-w-0 items-center gap-1.5 truncate">
              {virtual
                  ? t("files.computerFooter", {
                      folders: t("common.folder", { count: computer?.dirs.length ?? 0 }),
                      disks: t("files.disks", { count: computer?.volumes.length ?? 0 }),
                    })
                  : trashLocked
                    ? t("files.trashAsFinder")
                    : searchMode
                      ? searching
                        ? t("files.searchingIn", { folder: baseName(nav.location), count })
                        : results?.truncated
                          ? t("files.resultsMore", { count, folder: baseName(nav.location) })
                          : t("files.results", { count, folder: baseName(nav.location) })
                      : t("common.item", { count })}
              {nav.selection.length > 1 ? (
                <span className="text-muted">
                  {" "}
                  · {t("files.selectedOf", { selected: nav.selection.length, count })}
                </span>
              ) : (
                selectedEntry && selectedEntry.path !== nav.location && <span className="text-muted"> · {selectedEntry.name}</span>
              )}
            </span>
            <span className="flex flex-none items-center gap-4">
              {showHidden ? t("files.hiddenShown") : ""}
              {view === "icons" && !virtual && <IconSizeSlider value={iconSize} onChange={setIconSize} />}
              <button
                aria-label={env.terminalShown ? t("files.hideTerminal") : t("files.showTerminal")}
                title={env.terminalShown ? t("files.terminalTitleHide", { keys: TERMINAL_KEYS }) : t("files.terminalTitleShow", { keys: TERMINAL_KEYS })}
                onClick={env.toggleTerminal}
                className={`-mr-2 grid h-6 w-6 place-items-center rounded-md transition-colors hover:bg-white/8 hover:text-ink ${env.terminalShown ? "accent-fg" : "text-dim"}`}
              >
                <SquareTerminal className="h-3.5 w-3.5" />
              </button>
            </span>
          </footer>
        )}
      </section>
      {sidePreview &&
        env.previewSlot &&
        createPortal(
          // A panel of its own, the same size in every view: at most a quarter of the window.
          <div className="w-[min(20rem,25vw)] flex-none">
            {selectedEntries.length > 1 ? (
              <MultiPreview entries={selectedEntries} />
            ) : (
              <PreviewPane entry={previewEntry} platform={boot.platform} folder={previewFolder} onQuickLook={() => setQuickLook(true)} />
            )}
          </div>,
          env.previewSlot,
        )}
      {active && quickLook && selectedEntry && <QuickLook entry={selectedEntry} platform={boot.platform} onClose={() => setQuickLook(false)} />}
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          alt={menu.alt}
          items={menu.items ?? (inTrash ? trashMenuItems(menu.entry ?? null) : menuItems(menu.entry ?? null, menu.dir ?? nav.location))}
          onClose={() => setMenu(null)}
        />
      )}
      {dropdown && dropdownRect && (
        <ContextMenu
          x={dropdown.kind === "more" ? dropdownRect.right : dropdownRect.left}
          y={dropdownRect.bottom + 6}
          items={dropdown.kind === "organize" ? organizeItems() : moreItems()}
          anchor={dropdown.anchor}
          onClose={() => setDropdown(null)}
        />
      )}
    </>
  );
});
