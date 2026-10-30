import type { TaskProgress } from "./api";
import { bytesText, duration } from "../ui/format";
import { t, type Key } from "../i18n";

/** What a file operation does (its icon and its words). */
export type TaskKind = "copy" | "move" | "duplicate" | "trash" | "delete" | "compress" | "extract" | "empty" | "put-back" | "permissions" | "metadata";

let lastTaskId = 0;
/** A new id for a backend task (operations, and the measures the dialogs ask for). */
export const newTaskId = () => ++lastTaskId;

export type TaskState = "running" | "done" | "failed" | "cancelled";

/** One file operation as the window follows it. */
export interface OpTask {
  id: number;
  kind: TaskKind;
  /** "Copying 3 items". */
  title: string;
  sources: string[];
  /** The folder the items go to (none for a delete). */
  dest: string | null;
  state: TaskState;
  /** Asked to stop; it ends at its next chunk or entry. */
  cancelling: boolean;
  progress: TaskProgress | null;
  error: string | null;
  started: number;
  ended: number | null;
  /** Recent progress, for the speed: the last few seconds only. */
  samples: Sample[];
}

export interface Sample {
  t: number;
  bytes: number;
  files: number;
}

/** How far back the speed looks. */
const SPEED_WINDOW_MS = 4000;

/** Done so far, from 0 to 1, by bytes, else files, else items; null while
 * nothing tells (still measuring, or an extraction, whose size comes last). */
export function fraction(task: Pick<OpTask, "state" | "progress">): number | null {
  if (task.state === "done") return 1;
  const p = task.progress;
  if (!p || p.preparing) return null;
  const ratio = (done: number, total: number) => Math.min(1, done / total);
  if (p.bytes_total > 0) return ratio(p.bytes_done, p.bytes_total);
  if (p.files_total > 0) return ratio(p.files_done, p.files_total);
  if (p.items_total > 0 && p.items_done > 0) return ratio(p.items_done, p.items_total);
  return null;
}

/** The samples with a new one, dropping those older than the window. */
export function withSample(samples: Sample[], t: number, p: TaskProgress): Sample[] {
  return [...samples.filter((s) => t - s.t <= SPEED_WINDOW_MS), { t, bytes: p.bytes_done, files: p.files_done }];
}

/** Bytes (or, with no bytes to count, files) per second over the samples; null until there is a span to measure. */
export function rate(samples: Sample[], byBytes: boolean): number | null {
  if (samples.length < 2) return null;
  const first = samples[0];
  const last = samples[samples.length - 1];
  const secs = (last.t - first.t) / 1000;
  if (secs < 0.5) return null;
  const done = byBytes ? last.bytes - first.bytes : last.files - first.files;
  return done > 0 ? done / secs : null;
}

/** Seconds left at the current speed; null when unknown. */
export function secondsLeft(task: OpTask): number | null {
  const p = task.progress;
  if (!p || p.preparing || task.state !== "running") return null;
  const byBytes = p.bytes_total > 0;
  const speed = rate(task.samples, byBytes);
  if (!speed) return null;
  const left = byBytes ? p.bytes_total - p.bytes_done : p.files_total - p.files_done;
  return left > 0 ? left / speed : null;
}

/** The line under the bar: how much, and how long to go. */
export function progressText(task: OpTask): string {
  const p = task.progress;
  switch (task.state) {
    case "done":
      return t("tasks.done");
    case "cancelled":
      return t("tasks.cancelled");
    case "failed":
      return task.error ?? t("tasks.failed");
  }
  if (task.cancelling) return t("tasks.cancelling");
  if (!p || p.preparing) return t("tasks.preparing");
  const left = secondsLeft(task);
  const tail = left == null ? "" : ` · ${left < 5 ? t("tasks.fewSecondsLeft") : t("tasks.left", { time: duration(left) })}`;
  if (p.bytes_total > 0) return `${t("tasks.bytesOf", { done: bytesText(p.bytes_done), total: bytesText(p.bytes_total) })}${tail}`;
  if (p.files_total > 0) return `${t("tasks.filesOf", { done: p.files_done, count: p.files_total })}${tail}`;
  if (p.items_total > 0 && p.items_done > 0) return t("tasks.itemsOf", { done: p.items_done, count: p.items_total });
  // An extraction: its entries show as they come.
  if (p.files_done > 0) return t("tasks.filesSoFar", { count: p.files_done });
  return t("tasks.working");
}

const TITLES: Record<TaskKind, Key> = {
  copy: "tasks.title.copy",
  move: "tasks.title.move",
  duplicate: "tasks.title.duplicate",
  trash: "tasks.title.trash",
  delete: "tasks.title.delete",
  compress: "tasks.title.compress",
  extract: "tasks.title.extract",
  empty: "tasks.title.empty",
  "put-back": "tasks.title.putBack",
  permissions: "tasks.title.permissions",
  metadata: "tasks.title.metadata",
};

/** The words for an operation of `count` items, under way. */
export function titleFor(kind: TaskKind, count: number, name?: string): string {
  const what = count === 1 && name ? `"${name}"` : t("common.item", { count });
  return t(TITLES[kind], { what });
}

/** A path as the user knows it: the home folder as `~`. */
export function tildePath(path: string, home: string): string {
  if (home !== "/" && (path === home || path.startsWith(`${home}/`))) return `~${path.slice(home.length)}`;
  return path;
}
