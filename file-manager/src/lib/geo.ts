/** Web Mercator, as map tiles are cut: 256-pixel tiles, 2^zoom of them across. */
export const TILE = 256;

/** Where a point lies, in pixels of the whole world at `zoom`. */
export function worldPixel(lat: number, lon: number, zoom: number): { x: number; y: number } {
  const size = TILE * 2 ** zoom;
  const clamped = Math.max(-85.05112878, Math.min(85.05112878, lat));
  const s = Math.sin((clamped * Math.PI) / 180);
  return { x: ((lon + 180) / 360) * size, y: (0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)) * size };
}

/** The point at a world pixel (the inverse of `worldPixel`). */
export function latLonAt(x: number, y: number, zoom: number): { lat: number; lon: number } {
  const size = TILE * 2 ** zoom;
  const lon = (x / size) * 360 - 180;
  const n = Math.PI - (2 * Math.PI * y) / size;
  return { lat: (180 / Math.PI) * Math.atan(Math.sinh(n)), lon };
}

/** The tiles covering a view of `width` x `height` pixels centered on world pixel (cx, cy):
 * each with where it goes in the view. Columns wrap around the world; rows past the poles are left out. */
export function tilesFor(cx: number, cy: number, zoom: number, width: number, height: number): { x: number; y: number; z: number; left: number; top: number }[] {
  const n = 2 ** zoom;
  const x0 = Math.floor((cx - width / 2) / TILE);
  const x1 = Math.floor((cx + width / 2) / TILE);
  const y0 = Math.max(0, Math.floor((cy - height / 2) / TILE));
  const y1 = Math.min(n - 1, Math.floor((cy + height / 2) / TILE));
  const out = [];
  for (let ty = y0; ty <= y1; ty++) {
    for (let tx = x0; tx <= x1; tx++) {
      out.push({ x: ((tx % n) + n) % n, y: ty, z: zoom, left: Math.round(tx * TILE - (cx - width / 2)), top: Math.round(ty * TILE - (cy - height / 2)) });
    }
  }
  return out;
}

/** 25.4419 -> 25°26'31" S style, from decimal degrees. */
export function dms(value: number, positive: string, negative: string): string {
  const v = Math.abs(value);
  const d = Math.floor(v);
  const m = Math.floor((v - d) * 60);
  const s = Math.round(((v - d) * 60 - m) * 60);
  const [mm, ss] = s === 60 ? [m + 1, 0] : [m, s];
  return `${d}°${String(mm).padStart(2, "0")}'${String(ss).padStart(2, "0")}" ${value < 0 ? negative : positive}`;
}
