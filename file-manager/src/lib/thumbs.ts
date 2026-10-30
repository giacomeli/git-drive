import { useEffect, useState, type RefObject } from "react";
import { api, fileUrl, type Entry } from "./api";
import { pickNext, type JobMeta } from "./thumbQueue";

/** Thumbnails kept for the session; past this, the least recently used are released. */
const MAX_KEPT = 1200;

/** A job of the queue: a thumbnail, or a folder's scan for its previews. */
interface Job {
  meta: JobMeta;
  run: () => Promise<unknown>;
  listeners: Set<(result: unknown) => void>;
  /** Called when the last listener leaves while it runs (a scan can stop early). */
  onCancelRunning?: () => void;
  started: boolean;
  /** Told to stop: whoever asks again gets a new job, not this one's cancelled answer. */
  stopping: boolean;
  finished: boolean;
}

/** key -> object URL, or null when the file has no thumbnail. Insertion order is recency. */
const done = new Map<string, string | null>();
/** Waiting or running. */
const jobs = new Map<string, Job>();
/** Keys not started yet. */
const waiting: string[] = [];
const running = new Set<Job>();
let seq = 0;

/** Pixels asked of the backend for an icon of `px` CSS pixels, in steps so a
 * slider drag reuses what was drawn. */
export function thumbPixels(px: number): number {
  const need = px * (window.devicePixelRatio || 1);
  return need <= 128 ? 128 : need <= 256 ? 256 : 512;
}

/** A changed file (new mtime or size) gets a new thumbnail; one drawn without Quick Look is another. */
export const thumbKey = (entry: Entry, size: number, quicklook = true) =>
  `${entry.path}\n${entry.modified ?? 0}\n${entry.size}\n${size}${quicklook ? "" : "\nraster"}`;

function mimeOf(bytes: ArrayBuffer): string {
  const head = new Uint8Array(bytes, 0, Math.min(2, bytes.byteLength));
  return head[0] === 0xff && head[1] === 0xd8 ? "image/jpeg" : "image/png";
}

function remember(key: string, url: string | null) {
  done.delete(key);
  done.set(key, url);
  while (done.size > MAX_KEPT) {
    const [oldKey, oldUrl] = done.entries().next().value as [string, string | null];
    done.delete(oldKey);
    if (oldUrl) URL.revokeObjectURL(oldUrl);
  }
}

/** Whether a thumbnail handed out before is still held (an evicted URL is revoked). */
export const thumbAlive = (key: string, url: string) => done.get(key) === url;

/** Starts what the queue allows (`pickNext`: files before folders, newest first, within the limits). */
function pump() {
  for (;;) {
    const i = pickNext(
      waiting.map((k) => jobs.get(k)!.meta),
      [...running].map((j) => j.meta),
    );
    if (i < 0) return;
    const key = waiting.splice(i, 1)[0];
    const job = jobs.get(key)!;
    job.started = true;
    running.add(job);
    job
      .run()
      .catch(() => null)
      .then((result) => {
        job.finished = true;
        running.delete(job);
        // A stopped job may have been replaced under its key meanwhile.
        if (jobs.get(key) === job) jobs.delete(key);
        job.listeners.forEach((l) => l(result));
        pump();
      });
  }
}

/** Queues `run` under `key` (one job per key, shared by its listeners); the
 * returned function withdraws the listener: a job nobody waits for is
 * dropped if it has not started, or told to stop if it runs. */
function submit(key: string, meta: Omit<JobMeta, "seq">, run: () => Promise<unknown>, listener: (result: unknown) => void, onCancelRunning?: () => void): () => void {
  let job = jobs.get(key);
  if (!job || job.stopping) {
    job = { meta: { ...meta, seq: ++seq }, run, listeners: new Set(), onCancelRunning, started: false, stopping: false, finished: false };
    jobs.set(key, job);
    waiting.push(key);
  } else if (!job.started) {
    // Asked again: it is the most recent now.
    job.meta.seq = ++seq;
  }
  job.listeners.add(listener);
  const j = job;
  pump();
  return () => {
    if (j.finished || !j.listeners.delete(listener) || j.listeners.size > 0) return;
    if (!j.started) {
      waiting.splice(waiting.indexOf(key), 1);
      jobs.delete(key);
    } else if (j.onCancelRunning && !j.stopping) {
      j.stopping = true;
      j.onCancelRunning();
    }
  };
}

/** Asks for `entry`'s thumbnail of `size` px; `listener` gets its URL (null: it keeps its icon). */
export function requestThumb(
  entry: Entry,
  size: number,
  opts: { cls: JobMeta["cls"]; lane: JobMeta["lane"]; quicklook: boolean },
  listener: (url: string | null) => void,
): () => void {
  const key = thumbKey(entry, size, opts.quicklook);
  if (done.has(key)) {
    const url = done.get(key)!;
    remember(key, url);
    listener(url);
    return () => {};
  }
  return submit(
    key,
    { cls: opts.cls, lane: opts.lane },
    () =>
      api
        .thumbnail(entry.path, size, opts.quicklook)
        .then((bytes) => (bytes.byteLength > 0 ? URL.createObjectURL(new Blob([bytes], { type: mimeOf(bytes) })) : null))
        // A file that cannot be drawn keeps its icon.
        .catch(() => null)
        .then((url) => {
          remember(key, url);
          return url;
        }),
    (u) => listener(u as string | null),
  );
}

/** Queues a folder's scan (class "folder"); `onCancelRunning` stops it in the backend when nobody waits any more. */
export function enqueueScan<T>(key: string, lane: JobMeta["lane"], run: () => Promise<T>, listener: (result: T | null) => void, onCancelRunning: () => void): () => void {
  return submit(key, { cls: "folder", lane }, run, (r) => listener(r as T | null), onCancelRunning);
}

function request(entry: Entry, size: number, listener: (url: string | null) => void): () => void {
  return requestThumb(entry, size, { cls: "file", lane: "local", quicklook: true }, listener);
}

// One observer for every tile; the scroller's clipping counts, so a tile is
// "in view" only when it is on screen. An element may have several listeners
// (its thumbnail, a folder's previews).
const onVisibility = new WeakMap<Element, Set<(visible: boolean) => void>>();
let observer: IntersectionObserver | null = null;
function sharedObserver(): IntersectionObserver {
  observer ??= new IntersectionObserver((records) => {
    for (const r of records) onVisibility.get(r.target)?.forEach((l) => l(r.isIntersecting));
  });
  return observer;
}

export function useInView(ref: RefObject<Element | null>): boolean {
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const o = sharedObserver();
    let set = onVisibility.get(el);
    if (!set) {
      set = new Set();
      onVisibility.set(el, set);
      o.observe(el);
    }
    set.add(setVisible);
    return () => {
      const s = onVisibility.get(el);
      s?.delete(setVisible);
      if (s && s.size === 0) {
        o.unobserve(el);
        onVisibility.delete(el);
      }
    };
  }, [ref]);
  return visible;
}

/** SVG up to this size is drawn by the webview from the file itself. */
const SVG_MAX_BYTES = 2 << 20;

/** The webview draws SVG itself, transparent (Quick Look paints it on white). */
export const ownSvg = (entry: Entry) => entry.ext === "svg" && entry.size <= SVG_MAX_BYTES;

/** The thumbnail URL of `entry` once its tile has been on screen; null until
 * then, and for files that keep their icon. */
export function useThumbnail(entry: Entry, px: number, ref: RefObject<Element | null>): string | null {
  const visible = useInView(ref);
  const svg = ownSvg(entry);
  const size = thumbPixels(px);
  const [url, setUrl] = useState<{ key: string; url: string | null } | null>(null);
  const key = thumbKey(entry, size);
  useEffect(() => {
    if (!visible) return;
    if (svg) return void setUrl({ key, url: fileUrl(entry.path) });
    if (!entry.thumb) return;
    return request(entry, size, (u) => setUrl({ key, url: u }));
    // `key` covers the entry's identity and the size.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, visible, entry.thumb]);
  // A thumbnail of the previous size stays up until the new one is drawn.
  return url?.url ?? null;
}
