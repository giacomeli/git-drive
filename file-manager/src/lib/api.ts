import { Channel, convertFileSrc, invoke } from "@tauri-apps/api/core";

// Mirrors of the Rust types (src-tauri/src/fsmodel.rs and main.rs).

export type EntryKind = "dir" | "file" | "symlink";

export interface Entry {
  name: string;
  path: string;
  kind: EntryKind;
  /** A folder, or a symlink to one. */
  is_dir: boolean;
  size: number;
  /** Unix seconds. */
  modified: number | null;
  /** Unix seconds; null where the file system keeps no birth time. */
  created: number | null;
  hidden: boolean;
  /** Lowercase extension; empty for folders. */
  ext: string;
  /** The icon view can ask for a thumbnail of it. */
  thumb: boolean;
}

export interface Listing {
  path: string;
  parent: string | null;
  entries: Entry[];
}

export type PreviewKind = "directory" | "image" | "video" | "audio" | "pdf" | "text" | "markdown" | "json" | "model" | "other";

export interface TextPreview {
  text: string;
  truncated: boolean;
  /** Binary data follows: the text stops before it. */
  binary: boolean;
  bytes: number;
}

/** End of a search; the matches themselves arrive in batches while it runs. */
export interface SearchSummary {
  truncated: boolean;
  cancelled: boolean;
  visited: number;
}

export type PlaceIcon =
  | "drive"
  | "home"
  | "desktop"
  | "documents"
  | "downloads"
  | "pictures"
  | "movies"
  | "music"
  | "folder"
  | "disk"
  | "trash";

/** A folder the sidebar or the Computer view links to. */
export interface Place {
  label: string;
  path: string;
  icon: PlaceIcon;
  /** A bookmark whose folder is gone (renamed, deleted, unmounted). */
  missing: boolean;
}

/** The drive's repository against what its host accepts (the daemon measures it). */
export interface RepoUsage {
  /** Bytes of the object store: what the remote holds, give or take. */
  used: number;
  measured_at: number;
  /** "GitHub", "GitLab.com", or the remote's host name. */
  host: string;
  /** The host's recommended maximum, or the user's quota; null when unknown. */
  limit: number | null;
  /** A lower size the host strongly recommends staying under. */
  recommended: number | null;
  /** The limit is `repo_quota_gb` from the config. */
  from_config: boolean;
}

/** A mounted volume, with its space in bytes (0 when unknown). */
export interface Volume {
  name: string;
  path: string;
  kind: "system" | "drive" | "external";
  total: number;
  free: number;
  /** The drive's repository; null for other volumes, or while the daemon has not measured it. */
  repo: RepoUsage | null;
}

export interface SidebarData {
  bookmarks: Place[];
  volumes: Volume[];
  /** The folder the Trash shows. */
  trash: string;
}

export interface ComputerData {
  /** The home's standard folders (Desktop, Documents, Pictures...). */
  dirs: Place[];
  volumes: Volume[];
}

export interface Boot {
  start: string;
  /** Where the column view and the path bar start: the drive, the home folder, the volumes. */
  places: Place[];
  /** Folders never renamed, moved or deleted from here. */
  fixed: string[];
  accent: string;
  glass: "liquid-glass" | "vibrancy" | "none";
  platform: "macOS" | "Linux";
  /** bsdtar is here: Compress and Extract Here show in the menu. */
  archives: boolean;
  /** The system's preferred languages ("pt-BR", "en-US" ...), the most wanted first. */
  locales: string[];
  /** For About: this app's version, the processor it was built for, Tauri's version. */
  version: string;
  arch: string;
  tauri: string;
}

/** An app that opens a file (Open With). */
export interface AppChoice {
  /** The app bundle's path (macOS) or its desktop file id (Linux). */
  id: string;
  name: string;
  /** Its icon as a PNG data URL; null when none could be drawn. */
  icon: string | null;
  /** The app the system opens this file with. */
  default: boolean;
}

/** A move to the Trash: done, or `remaining` (not touched) can only be deleted for good. */
export type Trashed = { outcome: "done" } | { outcome: "no_trash"; reason: string; remaining: string[] };

export const api = {
  boot: () => invoke<Boot>("boot"),
  /** The accent saved in the shared preferences (the Control Center may have changed it). */
  accent: () => invoke<string>("accent"),
  saveAccent: (accent: string) => invoke<void>("save_accent", { accent }),
  listDir: (path: string, showHidden: boolean) => invoke<Listing>("list_dir", { path, showHidden }),
  previewKind: (path: string) => invoke<PreviewKind>("preview_kind", { path }),
  /** The file as text: `max` bytes at most (the backend's default, 256 KB, when absent). */
  readText: (path: string, max?: number) => invoke<TextPreview>("read_text", { path, max }),
  /** PNG or JPEG bytes; empty when the file keeps its icon. */
  /** `quicklook` false: decoded in the app only (the drive's folder previews: Quick Look writes a PNG to disk). */
  thumbnail: (path: string, size: number, quicklook = true) => invoke<ArrayBuffer>("thumbnail", { path, size, quicklook }),
  /** Up to four files of a folder for the previews on its icon (the backend's rules for the place). */
  folderPreview: (path: string, request: number) => invoke<FolderPicks>("folder_preview", { path, request }),
  cancelFolderPreview: (request: number) => invoke<void>("cancel_folder_preview", { request }),
  search: (root: string, query: string, showHidden: boolean, onFound: (batch: Entry[]) => void) => {
    const found = new Channel<Entry[]>();
    found.onmessage = onFound;
    return invoke<SearchSummary>("search", { root, query, showHidden, found });
  },
  sidebar: () => invoke<SidebarData>("sidebar"),
  /** Saves the bookmarks in this order; returns them as the sidebar shows them. */
  saveBookmarks: (paths: string[]) => invoke<Place[]>("save_bookmarks", { paths }),
  computer: () => invoke<ComputerData>("computer"),
  /** Puts an item of the Trash back where it came from (Linux); its new path. */
  putBack: (path: string) => invoke<string>("put_back", { path }),
  /** How many items went. */
  emptyTrash: (task: number) => invoke<number>("empty_trash", { task }),
  /** The `.app` this window runs from (macOS); null outside a bundle. */
  appBundle: () => invoke<string | null>("app_bundle"),
  /** System Settings at Privacy & Security, Full Disk Access (macOS). */
  openFullDiskAccess: () => invoke<void>("open_full_disk_access"),
  /** The apps that open `path`, its default first. */
  openWithApps: (path: string) => invoke<AppChoice[]>("open_with_apps", { path }),
  /** Opens `paths` with the app `id`; `always` also makes it the default for the first one's type. */
  openWith: (paths: string[], id: string, always: boolean) => invoke<void>("open_with", { paths, id, always }),
  /** Zips `paths` beside them; the archive's path. */
  compress: (paths: string[], task: number) => invoke<string>("compress", { paths, task }),
  /** Extracts an archive beside it; where its items landed. */
  extract: (path: string, task: number) => invoke<string[]>("extract", { path, task }),
  setDesktopPicture: (path: string) => invoke<void>("set_desktop_picture", { path }),
  /** Starts the user's shell in `cwd`; its output arrives raw (bytes) as it comes, then its exit code. */
  termOpen: (cwd: string, cols: number, rows: number, onData: (bytes: Uint8Array) => void, onExit: (code: number | null) => void) => {
    const data = new Channel<ArrayBuffer | number[]>();
    data.onmessage = (m) => onData(m instanceof ArrayBuffer ? new Uint8Array(m) : Uint8Array.from(m));
    const exit = new Channel<number | null>();
    exit.onmessage = onExit;
    return invoke<number>("term_open", { cwd, cols, rows, onData: data, onExit: exit });
  },
  termWrite: (id: number, data: string) => invoke<void>("term_write", { id, data }),
  /** The new size; `redraw` (the width changed) has the shell redraw its screen when it is at its prompt. */
  termResize: (id: number, cols: number, rows: number, redraw = false) => invoke<void>("term_resize", { id, cols, rows, redraw }),
  termClose: (id: number) => invoke<void>("term_close", { id }),
  /** The shell's working folder now. */
  termCwd: (id: number) => invoke<string | null>("term_cwd", { id }),
  /** The shell's folder, and the server an `ssh` in front of it is connected to. */
  termStatus: (id: number) => invoke<{ cwd: string | null; ssh: string | null; command: string | null } | null>("term_status", { id }),
  /** Types the paths into the shell, quoted where needed. */
  termPastePaths: (id: number, paths: string[]) => invoke<void>("term_paste_paths", { id, paths }),
  /** Types `cd` to `path` and Return. */
  termCd: (id: number, path: string) => invoke<void>("term_cd", { id, path }),
  /** For each path: a folder (true), a file (false), or not there (null). */
  pathKinds: (paths: string[]) => invoke<(boolean | null)[]>("path_kinds", { paths }),
  /** Opens an http(s) link in the browser. */
  openUrl: (url: string) => invoke<void>("open_url", { url }),
  openPath: (path: string, reveal = false) => invoke<void>("open_path", { path, reveal }),
  /** The new folder's path; rejects when the name is taken or invalid. */
  newFolder: (parent: string, name: string) => invoke<string>("new_folder", { parent, name }),
  /** The new path; rejects with the reason when the name is taken or invalid. */
  rename: (path: string, name: string) => invoke<string>("rename", { path, name }),
  /** Copies, or moves when `cut`, into `dest`; where each item landed. */
  paste: (sources: string[], dest: string, cut: boolean, task: number) => invoke<string[]>("paste", { sources, dest, cut, task }),
  trash: (paths: string[], task: number) => invoke<Trashed>("trash", { paths, task }),
  /** Drags `paths` out of the window; resolves when the drag ends, with whether something took them. */
  startDrag: (paths: string[]) => invoke<boolean>("start_drag", { paths }),
  /** What a drop of `sources` on `dest` does, from the keys held and the volumes. */
  dropOperation: (sources: string[], dest: string) => invoke<"move" | "copy">("drop_operation", { sources, dest }),
  deletePermanently: (paths: string[], task: number) => invoke<void>("delete_permanently", { paths, task }),
  /** The window's appearance: `light` forces one (null follows the system); the glass takes the theme shown. */
  setAppearance: (light: boolean | null, effectiveLight: boolean) => invoke<void>("set_appearance", { light, effectiveLight }),
  /** The language of the backend's messages. */
  setLanguage: (language: string) => invoke<void>("set_language", { language }),
  /** The progress of every file operation under way, by the id given when it started. */
  taskProgress: () => invoke<Record<string, TaskProgress>>("task_progress"),
  /** Stops a file operation at its next chunk or entry; it then rejects with "Cancelled.". */
  cancelTask: (id: number) => invoke<void>("cancel_task", { id }),
  properties: (paths: string[]) => invoke<ItemInfo[]>("properties", { paths }),
  /** Bytes and entries `paths` hold in all (folders walked). */
  totalSize: (paths: string[], task: number) => invoke<[number, number]>("total_size", { paths, task }),
  /** Sets and clears permission bits; with `recursive`, through folders too. How many changed. */
  setMode: (paths: string[], change: { set: number; clear: number }, recursive: boolean, task: number) =>
    invoke<number>("set_mode", { paths, change, recursive, task }),
  removeQuarantine: (path: string) => invoke<void>("remove_quarantine", { path }),
  imageMeta: (path: string) => invoke<ImageMeta | null>("image_meta", { path }),
  writeImageMeta: (paths: string[], edit: MetaEdit, task: number) => invoke<number>("write_image_meta", { paths, edit, task }),
  /** Where a typed path leads (`~` is home, relative to `base`); rejects with what is not there. */
  resolvePath: (path: string, base: string) => invoke<{ path: string; is_dir: boolean }>("resolve_path", { path, base }),
};

/** A file operation's progress: the bar follows the bytes, else the files, else the items. */
export interface TaskProgress {
  /** Still measuring what there is to do. */
  preparing: boolean;
  bytes_done: number;
  bytes_total: number;
  files_done: number;
  files_total: number;
  items_done: number;
  items_total: number;
  /** The entry it is on. */
  current: string | null;
}

/** What `lstat` says of an item, in the Properties dialog's terms. */
export interface ItemInfo {
  path: string;
  name: string;
  kind: "file" | "dir" | "symlink" | "other";
  size: number;
  on_disk: number;
  created: number | null;
  modified: number | null;
  accessed: number | null;
  changed: number | null;
  /** Permission bits with the special ones. */
  mode: number;
  uid: number;
  gid: number;
  owner: string | null;
  group: string | null;
  /** The user running the app owns it (or is root). */
  mine: boolean;
  links: number;
  inode: number;
  device: number;
  link_target: string | null;
  link_broken: boolean;
  xattrs: { name: string; size: number }[];
}

export interface Fact {
  label: string;
  value: string;
}

/** An image's metadata, from its EXIF and XMP (or its XMP sidecar). */
export interface ImageMeta {
  format: string;
  width: number | null;
  height: number | null;
  title: string | null;
  description: string | null;
  keywords: string[];
  creator: string | null;
  copyright: string | null;
  rating: number | null;
  /** "YYYY-MM-DDTHH:MM:SS". */
  taken: string | null;
  taken_offset: string | null;
  camera: Fact[];
  exposure: Fact[];
  image: Fact[];
  gps: { lat: number; lon: number; altitude: number | null; direction: number | null } | null;
  has_location: boolean;
  editing: { descriptive: "embedded" | "sidecar" | "none"; privacy: boolean; note: string | null };
  sidecar: string | null;
}

/** A change of image metadata: each field present is set ("" clears it). */
export interface MetaEdit {
  title?: string;
  description?: string;
  creator?: string;
  copyright?: string;
  keywords?: string[];
  add_keywords?: string[];
  /** 0 clears it. */
  rating?: number;
  taken?: string;
  remove_location?: boolean;
  strip_all?: boolean;
}

/** A folder's previews: up to four of its files, and how to draw them. */
export interface FolderPicks {
  entries: Entry[];
  /** Ask for these thumbnails without Quick Look (the drive). */
  raster_only: boolean;
  scanned: number;
  truncated: boolean;
  cancelled: boolean;
}

/** The message a cancelled operation rejects with. */
export const CANCELLED = "Cancelled.";

/** URL the webview loads a file from (asset protocol, byte ranges for media). */
export const fileUrl = (path: string) => convertFileSrc(path);

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}

export function parentOf(path: string): string | null {
  if (path === "/") return null;
  const idx = path.replace(/\/+$/, "").lastIndexOf("/");
  return idx <= 0 ? "/" : path.slice(0, idx);
}

export function baseName(path: string): string {
  if (path === "/") return "/";
  return path.replace(/\/+$/, "").split("/").pop() ?? path;
}
