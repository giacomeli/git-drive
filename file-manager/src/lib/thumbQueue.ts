/** A queued thumbnail or folder scan: its class, where it reads, and when it was asked. */
export interface JobMeta {
  seq: number;
  /** A file's own thumbnail, or a folder's previews (its scan and its pictures). */
  cls: "file" | "folder";
  /** The drive (a FUSE loop that serves one request at a time) or anywhere else. */
  lane: "drive" | "local";
}

/** How many run at once: in all, folder work outside the drive, folder work on the drive. */
export const QUEUE_LIMITS = { total: 4, folderLocal: 2, folderDrive: 1 } as const;

/** Which waiting job starts next (its index), or -1: files before folders,
 * the newest first within a class (what just scrolled into view), within
 * the limits of what is running. */
export function pickNext(waiting: readonly JobMeta[], running: readonly JobMeta[], limits = QUEUE_LIMITS): number {
  if (running.length >= limits.total) return -1;
  const folders = (lane: JobMeta["lane"]) => running.filter((r) => r.cls === "folder" && r.lane === lane).length;
  const room = (j: JobMeta) =>
    j.cls === "file" || (j.lane === "drive" ? folders("drive") < limits.folderDrive : folders("local") < limits.folderLocal);
  let best = -1;
  waiting.forEach((j, i) => {
    if (!room(j)) return;
    if (best < 0) return void (best = i);
    const b = waiting[best];
    if ((j.cls === "file" && b.cls === "folder") || (j.cls === b.cls && j.seq > b.seq)) best = i;
  });
  return best;
}
