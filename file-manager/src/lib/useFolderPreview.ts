import { useEffect, useState, useSyncExternalStore, type RefObject } from "react";
import { api, type Entry } from "./api";
import { DRIVE_SETTLE_MS, FOCUS_REVALIDATE_MIN_MS, laneOf, printCount, SETTLE_MS } from "./folderPreview";
import { enqueueScan, requestThumb, thumbAlive, thumbKey, thumbPixels, useInView } from "./thumbs";

/** A preview on a folder's icon: the picture and the file it shows. */
export interface Shown {
  url: string;
  entry: Entry;
  /** Its thumbnail's key (to know whether the URL is still held). */
  key: string;
}

// --- settings: on or off (Settings), and where the drive is (its own rules) ---

let enabled = true;
let onDrive = true;
let drive: string | null = null;
let generation = 0;
let lastFocusBump = 0;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

/** The window's settings (Settings: on or off, and on the drive or not) and
 * the drive's mount point (its folders get the drive's rules: fewer reads,
 * one scan at a time, no Quick Look). */
export function configureFolderPreviews(c: { enabled: boolean; onDrive: boolean; drive: string | null }) {
  if (c.enabled === enabled && c.onDrive === onDrive && c.drive === drive) return;
  enabled = c.enabled;
  onDrive = c.onDrive;
  drive = c.drive;
  notify();
}

/** Folders changed (an operation ended, the window came back): their previews
 * are checked again, shown as they were until the new ones are drawn. A
 * folder's date does not change when a file inside it is edited, so this is
 * how they learn. A window coming back to the front checks at most every
 * `FOCUS_REVALIDATE_MIN_MS`. */
export function invalidateFolderPreviews(why: "op" | "focus") {
  const now = Date.now();
  if (why === "focus") {
    if (now - lastFocusBump < FOCUS_REVALIDATE_MIN_MS) return;
    lastFocusBump = now;
  }
  generation++;
  notify();
}

// --- what was found and shown, by folder (a tile scrolled away and back shows them at once) ---

const SCANS_KEPT = 3000;
const scans = new Map<string, { stamp: string; picks: Entry[]; rasterOnly: boolean }>();
const shown = new Map<string, { stamp: string; size: number; prints: Shown[] }>();
/** Folders whose previews were seen: only a first appearance is animated. */
const seen = new Set<string>();
let nextRequest = 1;

// While anything scrolls, folders wait: a fast scroll asks for nothing, even
// for a tile that stayed on screen across a few steps.
let lastScroll = 0;
if (typeof document !== "undefined") document.addEventListener("scroll", () => (lastScroll = Date.now()), { capture: true, passive: true });

/** Runs `fn` once `ms` passed with no scroll in between; returns the cancel. */
function afterSettling(ms: number, fn: () => void): () => void {
  let timer = 0;
  const arm = (wait: number) => {
    timer = window.setTimeout(() => {
      const quiet = Date.now() - lastScroll;
      if (quiet < ms) arm(ms - quiet);
      else fn();
    }, wait);
  };
  arm(ms);
  return () => window.clearTimeout(timer);
}

function keep<V>(map: Map<string, V>, key: string, value: V) {
  map.delete(key);
  map.set(key, value);
  while (map.size > SCANS_KEPT) map.delete(map.keys().next().value as string);
}

/** Still drawable: every URL is held by the thumbnail cache. */
const alive = (prints: Shown[]) => prints.every((p) => thumbAlive(p.key, p.url));

/**
 * The previews of `entry` (a folder) at an icon of `iconSize` px, once its
 * tile has stayed on screen a moment: its scan and its pictures go through
 * the thumbnail queue (after the files' own thumbnails; one at a time on the
 * drive). Returns what to draw, and whether it is their first appearance.
 */
export function useFolderPreview(entry: Entry, iconSize: number, ref: RefObject<Element | null>): { prints: Shown[]; fresh: boolean } {
  const lane = laneOf(entry.path, useSyncExternalStore(subscribe, () => drive));
  const on = useSyncExternalStore(subscribe, () => enabled && (lane === "local" || onDrive));
  const gen = useSyncExternalStore(subscribe, () => generation);
  const visible = useInView(ref);
  const size = thumbPixels(iconSize);
  const stamp = `${entry.modified ?? 0}\n${gen}`;
  const cached = shown.get(entry.path);
  const [state, setState] = useState<{ prints: Shown[]; fresh: boolean } | null>(() =>
    cached && cached.size === size && alive(cached.prints) ? { prints: cached.prints, fresh: false } : null,
  );

  useEffect(() => {
    // Places that are no folder on disk (Computer) have nothing to scan.
    if (!on || !entry.is_dir || !visible || !entry.path.startsWith("/")) return;
    const now = shown.get(entry.path);
    if (now && now.stamp === stamp && now.size === size && alive(now.prints)) {
      setState((s) => (s?.prints === now.prints ? s : { prints: now.prints, fresh: false }));
      return;
    }
    let live = true;
    const withdraw: (() => void)[] = [];
    const draw = (picks: Entry[], rasterOnly: boolean) => {
      const n = printCount(iconSize, picks.length);
      if (n === 0) {
        keep(shown, entry.path, { stamp, size, prints: [] });
        if (live) setState({ prints: [], fresh: false });
        return;
      }
      const urls: (string | null)[] = new Array(n).fill(null);
      let left = n;
      picks.slice(0, n).forEach((p, i) =>
        withdraw.push(
          requestThumb(p, size, { cls: "folder", lane, quicklook: !rasterOnly }, (url) => {
            urls[i] = url;
            if (--left > 0) return;
            // Drawn together, once all have settled.
            const prints = urls.flatMap((u, k) => (u ? [{ url: u, entry: picks[k], key: thumbKey(picks[k], size, !rasterOnly) }] : []));
            keep(shown, entry.path, { stamp, size, prints });
            const first = prints.length > 0 && !seen.has(entry.path);
            seen.add(entry.path);
            if (live) setState({ prints, fresh: first });
          }),
        ),
      );
    };
    const stopWaiting = afterSettling(
      lane === "drive" ? DRIVE_SETTLE_MS : SETTLE_MS,
      () => {
        const scan = scans.get(entry.path);
        if (scan && scan.stamp === stamp) return draw(scan.picks, scan.rasterOnly);
        const request = nextRequest++;
        withdraw.push(
          enqueueScan(
            `scan\n${entry.path}\n${stamp}`,
            lane,
            () => api.folderPreview(entry.path, request),
            (picks) => {
              if (!picks || picks.cancelled) return;
              keep(scans, entry.path, { stamp, picks: picks.entries, rasterOnly: picks.raster_only });
              if (live) draw(picks.entries, picks.raster_only);
            },
            () => void api.cancelFolderPreview(request).catch(() => {}),
          ),
        );
      },
    );
    return () => {
      live = false;
      stopWaiting();
      withdraw.forEach((w) => w());
    };
    // `stamp` covers the folder's date and the generation.
  }, [on, visible, stamp, size, iconSize, lane, entry.path, entry.is_dir]);

  if (!on || !entry.is_dir) return { prints: [], fresh: false };
  return state ?? { prints: [], fresh: false };
}
