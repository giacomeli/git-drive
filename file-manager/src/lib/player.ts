// The video player's arithmetic, pure (tested in Node).

/** Seconds as `m:ss`, or `h:mm:ss` from an hour on. */
export function clockText(secs: number): string {
  const s = Math.max(0, Math.floor(Number.isFinite(secs) ? secs : 0));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const ss = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** The largest `w` x `h` box of the picture's proportions inside `room` (never larger than the room). */
export function fitInside(ratio: number, roomW: number, roomH: number): { width: number; height: number } {
  if (!(ratio > 0) || roomW <= 0 || roomH <= 0) return { width: roomW, height: roomH };
  return roomW / roomH > ratio ? { width: roomH * ratio, height: roomH } : { width: roomW, height: roomW / ratio };
}
