import {
  File,
  FileArchive,
  FileAudio,
  FileCode,
  FileImage,
  FileSpreadsheet,
  FileText,
  FileVideo,
  Folder,
  FolderSymlink,
  type LucideIcon,
} from "lucide-react";
import type { Entry } from "./api";
import { language, t, type Key } from "../i18n";

export type Family = "folder" | "image" | "video" | "audio" | "pdf" | "code" | "text" | "sheet" | "archive" | "other";

const FAMILY_BY_EXT: Record<string, Family> = {};
const add = (family: Family, exts: string) => exts.split(" ").forEach((e) => (FAMILY_BY_EXT[e] = family));
add("image", "png jpg jpeg gif webp bmp svg ico avif heic heif tif tiff psd");
add("video", "mp4 m4v mov webm mkv avi");
add("audio", "mp3 m4a aac wav ogg oga opus flac aif aiff");
add("pdf", "pdf");
add("code", "rs ts tsx js jsx mjs cjs py rb go java kt swift c h cpp hpp cc cs php sh bash zsh fish sql lua dart vue svelte html htm css scss json jsonc yaml yml toml xml gradle lock");
add("text", "txt md markdown rst log ini cfg conf env srt vtt tex rtf doc docx pages diff patch");
add("sheet", "csv tsv xls xlsx numbers ods");
add("archive", "zip tar gz tgz bz2 xz 7z rar dmg iso");

export function family(entry: Entry): Family {
  if (entry.is_dir) return "folder";
  return FAMILY_BY_EXT[entry.ext] ?? "other";
}

const ICON: Record<Family, LucideIcon> = {
  folder: Folder,
  image: FileImage,
  video: FileVideo,
  audio: FileAudio,
  pdf: FileText,
  code: FileCode,
  text: FileText,
  sheet: FileSpreadsheet,
  archive: FileArchive,
  other: File,
};

/** Tint per family; folders take the accent like Finder's blue folders. */
const TINT: Record<Family, string> = {
  folder: "var(--a2)",
  image: "#3fd0f0",
  video: "#ff8fc6",
  audio: "#c38bff",
  pdf: "#ff7a7a",
  code: "#7ee0a8",
  text: "#c3cae8",
  sheet: "#62ba46",
  archive: "#ffb547",
  other: "#a3a8cc",
};

export function iconFor(entry: Entry): { Icon: LucideIcon; color: string } {
  const fam = family(entry);
  const Icon = entry.kind === "symlink" && entry.is_dir ? FolderSymlink : ICON[fam];
  return { Icon, color: TINT[fam] };
}

/** The name of a file's kind, by its extension (what Finder's Kind column says). */
const KIND_KEY: Record<string, Key> = {
  png: "kinds.png",
  jpg: "kinds.jpeg",
  jpeg: "kinds.jpeg",
  gif: "kinds.gif",
  webp: "kinds.webp",
  svg: "kinds.svg",
  heic: "kinds.heic",
  mp4: "kinds.mp4",
  mov: "kinds.mov",
  webm: "kinds.webm",
  mp3: "kinds.mp3",
  m4a: "kinds.m4a",
  wav: "kinds.wav",
  flac: "kinds.flac",
  pdf: "kinds.pdf",
  md: "kinds.md",
  txt: "kinds.txt",
  json: "kinds.json",
  yaml: "kinds.yaml",
  yml: "kinds.yaml",
  toml: "kinds.toml",
  rs: "kinds.rs",
  ts: "kinds.ts",
  tsx: "kinds.tsx",
  js: "kinds.js",
  py: "kinds.py",
  sh: "kinds.sh",
  html: "kinds.html",
  css: "kinds.css",
  csv: "kinds.csv",
  zip: "kinds.zip",
  dmg: "kinds.dmg",
  log: "kinds.log",
};

export function kindLabel(entry: Entry): string {
  if (entry.is_dir) return t(entry.kind === "symlink" ? "kinds.alias" : "kinds.folder");
  if (!entry.ext) return t("kinds.document");
  const key = KIND_KEY[entry.ext];
  return key ? t(key) : t("kinds.extFile", { ext: entry.ext.toUpperCase() });
}

const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();

/** "Today, 14:05", "Yesterday, 09:12", "12 Oct 2026, 10:01" (as Finder writes them), in the app's language. */
export function dateLabel(secs: number | null): string {
  if (secs == null) return "--";
  const d = new Date(secs * 1000);
  const now = new Date();
  const time = d.toLocaleTimeString(language(), { hour: "2-digit", minute: "2-digit" });
  if (sameDay(d, now)) return t("kinds.today", { time });
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(d, yesterday)) return t("kinds.yesterday", { time });
  return t("kinds.dateTime", { date: d.toLocaleDateString(language(), { day: "numeric", month: "short", year: "numeric" }), time });
}

/** Archive endings bsdtar reads, longest first (mirrors archive.rs). */
const ARCHIVE_ENDINGS = [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".tbz2", ".tbz", ".txz", ".zip", ".tar", ".7z", ".rar"];

/** An archive's name without its ending (`backup.tar.gz` -> `backup`); null for anything else. */
export function archiveStem(name: string): string | null {
  const lower = name.toLowerCase();
  const ending = ARCHIVE_ENDINGS.find((e) => lower.endsWith(e));
  const stem = ending ? name.slice(0, name.length - ending.length) : "";
  return stem === "" ? null : stem;
}

/** Pictures macOS and GNOME take as a desktop picture. */
const WALLPAPER_EXTS = new Set(["jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "webp", "gif", "bmp", "jxl"]);

export const isWallpaper = (entry: Entry) => !entry.is_dir && WALLPAPER_EXTS.has(entry.ext);
