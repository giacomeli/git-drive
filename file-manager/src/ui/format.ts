// Copied from control-center/src/lib/format.ts (file manager POC, decision 2): keep the two
// in sync until the design system moves to a shared package. Numbers and words follow the
// app's language (the units stay as they are written everywhere).
import { num, t } from "../i18n";

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
  return { value: num(v, digits), unit: UNITS[i] };
}

export function bytesText(n: number | null | undefined): string {
  const b = bytes(n);
  return b.unit ? `${b.value} ${b.unit}` : b.value;
}

/** "12 s", "4 min", "3 h 05 min", "2 d 4 h". */
export function duration(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return t("format.seconds", { s: String(s) });
  const m = Math.floor(s / 60);
  if (m < 60) return t("format.minutes", { m: String(m) });
  const h = Math.floor(m / 60);
  if (h < 24) return t("format.hours", { h: String(h), m: String(m % 60).padStart(2, "0") });
  const d = Math.floor(h / 24);
  return t("format.days", { d: String(d), h: String(h % 24) });
}

export function ago(at: number | null | undefined, now: number): string {
  if (!at) return t("format.never");
  const secs = now - at;
  if (secs < 5) return t("format.justNow");
  return t("format.ago", { time: duration(secs) });
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
