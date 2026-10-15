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
  hidden: boolean;
  /** Lowercase extension; empty for folders. */
  ext: string;
}

export interface Listing {
  path: string;
  parent: string | null;
  entries: Entry[];
}

export type PreviewKind = "directory" | "image" | "video" | "audio" | "pdf" | "text" | "other";

export interface TextPreview {
  text: string;
  truncated: boolean;
  bytes: number;
}

/** End of a search; the matches themselves arrive in batches while it runs. */
export interface SearchSummary {
  truncated: boolean;
  cancelled: boolean;
  visited: number;
}

export interface Place {
  label: string;
  path: string;
  icon: "drive" | "home" | "desktop" | "documents" | "downloads" | "computer";
}

export interface Boot {
  start: string;
  places: Place[];
  accent: string;
  glass: "liquid-glass" | "vibrancy" | "none";
  platform: "macOS" | "Linux";
}

export interface Space {
  total: number;
  free: number;
}

export const api = {
  boot: () => invoke<Boot>("boot"),
  listDir: (path: string, showHidden: boolean) => invoke<Listing>("list_dir", { path, showHidden }),
  previewKind: (path: string) => invoke<PreviewKind>("preview_kind", { path }),
  readText: (path: string) => invoke<TextPreview>("read_text", { path }),
  search: (root: string, query: string, showHidden: boolean, onFound: (batch: Entry[]) => void) => {
    const found = new Channel<Entry[]>();
    found.onmessage = onFound;
    return invoke<SearchSummary>("search", { root, query, showHidden, found });
  },
  volume: (path: string) => invoke<Space | null>("volume", { path }),
  openPath: (path: string, reveal = false) => invoke<void>("open_path", { path, reveal }),
};

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
