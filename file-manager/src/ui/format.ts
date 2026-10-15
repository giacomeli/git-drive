// Copied from control-center/src/lib/format.ts (file manager POC, decision 2): keep the two
// in sync until the design system moves to a shared package.
const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** Bytes in decimal units, the way Finder and GNOME Files report sizes. */
export function bytes(n: number | null | undefined): { value: string; unit: string } {
  if (n == null || !Number.isFinite(n)) return { value: "-", unit: "" };
  let v = n;
  let i = 0;
  while (v >= 1000 && i < UNITS.length - 1) {
    v /= 1000;
    i++;
  }
  const digits = v >= 100 || i === 0 ? 0 : 1;
  return { value: v.toFixed(digits), unit: UNITS[i] };
}

export function bytesText(n: number | null | undefined): string {
  const b = bytes(n);
  return b.unit ? `${b.value} ${b.unit}` : b.value;
}

/** "12 s", "4 min", "3 h 05 min", "2 d 4 h". */
export function duration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} h ${String(m % 60).padStart(2, "0")} min`;
  const d = Math.floor(h / 24);
  return `${d} d ${h % 24} h`;
}

export function ago(at: number | null | undefined, now: number): string {
  if (!at) return "never";
  const secs = now - at;
  if (secs < 5) return "just now";
  return `${duration(secs)} ago`;
}

export function shortId(id: string): string {
  return id.slice(0, 8);
}

/** Last path component, for compact display of long paths. */
export function baseName(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

/** Replaces the home directory with "~". */
export function tildify(path: string): string {
  return path.replace(/^\/(Users|home)\/[^/]+/, "~");
}

/** Replaces every home directory inside a text (log messages) with "~". */
export function tildifyAll(text: string): string {
  return text.replace(/\/(Users|home)\/[^/\s]+/g, "~");
}
