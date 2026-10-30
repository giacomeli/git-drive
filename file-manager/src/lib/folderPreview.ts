/** A preview laid over a folder's icon: its center and box as fractions of
 * the icon's side, its tilt in degrees, and what is on top. */
export interface PrintBox {
  cx: number;
  cy: number;
  w: number;
  h: number;
  rotate: number;
  z: number;
}

/** How long a folder must stay on screen before its previews are asked for (a fast scroll asks nothing). */
export const SETTLE_MS = 180;
export const DRIVE_SETTLE_MS = 300;
/** A window coming back to the front checks the folders again at most this often. */
export const FOCUS_REVALIDATE_MIN_MS = 10_000;

/** How many previews an icon of `iconSize` px shows, of the `available` ones. */
export function printCount(iconSize: number, available: number): number {
  const max = iconSize <= 56 ? 1 : iconSize <= 80 ? 2 : 4;
  return Math.max(0, Math.min(max, available));
}

/** Where the previews go (the spec's table), for 1 to 4 of them; one takes
 * the picture's own proportions (`aspect`, width over height) within its box. */
export function printLayout(count: number, aspect = 4 / 3): PrintBox[] {
  switch (count) {
    case 1: {
      const [bw, bh] = [0.58, 0.44];
      const w = Math.min(bw, bh * aspect);
      return [{ cx: 0.5, cy: 0.6, w, h: w / aspect, rotate: -3, z: 1 }];
    }
    case 2:
      return [
        { cx: 0.38, cy: 0.6, w: 0.4, h: 0.4, rotate: -8, z: 1 },
        { cx: 0.62, cy: 0.62, w: 0.4, h: 0.4, rotate: 6, z: 2 },
      ];
    case 3:
      return [
        { cx: 0.31, cy: 0.63, w: 0.34, h: 0.34, rotate: -9, z: 1 },
        { cx: 0.69, cy: 0.63, w: 0.34, h: 0.34, rotate: 8, z: 2 },
        { cx: 0.5, cy: 0.56, w: 0.34, h: 0.34, rotate: -2, z: 3 },
      ];
    case 4:
      return [
        { cx: 0.34, cy: 0.46, w: 0.28, h: 0.28, rotate: -4, z: 1 },
        { cx: 0.66, cy: 0.46, w: 0.28, h: 0.28, rotate: 3, z: 2 },
        { cx: 0.34, cy: 0.71, w: 0.28, h: 0.28, rotate: 3, z: 3 },
        { cx: 0.66, cy: 0.71, w: 0.28, h: 0.28, rotate: -4, z: 4 },
      ];
    default:
      return [];
  }
}

/** The white frame of a print: 2.5% of the icon, between 1 and 4 px. */
export const frameWidth = (iconSize: number) => Math.max(1, Math.min(4, Math.round(iconSize * 0.025)));

/** The corners of a box after its tilt, as fractions of the icon (for the tests' bounds). */
export function corners(b: PrintBox): [number, number][] {
  const r = (b.rotate * Math.PI) / 180;
  return [
    [-1, -1],
    [1, -1],
    [1, 1],
    [-1, 1],
  ].map(([sx, sy]) => {
    const x = (sx * b.w) / 2;
    const y = (sy * b.h) / 2;
    return [b.cx + x * Math.cos(r) - y * Math.sin(r), b.cy + x * Math.sin(r) + y * Math.cos(r)];
  });
}

/** Whether `path` is inside the drive (its own path or below it). */
export function laneOf(path: string, drive: string | null): "drive" | "local" {
  return drive != null && (path === drive || path.startsWith(`${drive}/`)) ? "drive" : "local";
}
