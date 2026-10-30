import { api } from "./api";

/** The app's look: as the system has it, or one forced. */
export type Appearance = "system" | "light" | "dark";
export type Theme = "light" | "dark";

const KEY = "files.appearance";

/** The appearance saved (the Settings), read before the first paint. */
export function storedAppearance(): Appearance {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? '"system"');
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

const systemQuery = () => window.matchMedia?.("(prefers-color-scheme: light)");

/** The theme an appearance shows now. */
export function themeOf(a: Appearance): Theme {
  return a === "system" ? (systemQuery()?.matches ? "light" : "dark") : a;
}

/** Puts `a` on the page (`data-theme`, which the styles key on) and on the
 * window (its native glass and controls); returns the theme shown. */
export function applyAppearance(a: Appearance): Theme {
  const theme = themeOf(a);
  document.documentElement.dataset.theme = theme;
  api.setAppearance(a === "system" ? null : a === "light", theme === "light").catch(() => {});
  return theme;
}

/** Calls `onChange` when the system's appearance changes (the "system" choice follows it). */
export function onSystemChange(onChange: () => void): () => void {
  const q = systemQuery();
  if (!q) return () => {};
  q.addEventListener("change", onChange);
  return () => q.removeEventListener("change", onChange);
}
