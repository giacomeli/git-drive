// Accent colors of the Control Center, after the macOS accent picker.
// Each accent drives the theme through CSS variables:
//   --a1, --a2, --a3   gradient stops (hero card, primary buttons, toggles, charts)
//   --s1, --s2         secondary pair (other data in the disk ring, chart start, background glow)
//   --*-rgb            the same colors as "r g b" triplets, for alpha in shadows and tints
// Status colors (ok, pending, error, log levels) never follow the accent.

export interface Accent {
  id: string;
  label: string;
  /** What the picker swatch shows (macOS accent swatch color). */
  swatch: string;
  a1: string;
  a2: string;
  a3: string;
  s1: string;
  s2: string;
}

const NEUTRAL = { s1: "#c3cae8", s2: "#8790bd" };

export const ACCENTS: Accent[] = [
  {
    id: "multicolor",
    label: "Multicolor",
    swatch: "conic-gradient(from 200deg, #ff5257, #f7821b, #ffc600, #62ba46, #3fd0f0, #007aff, #a550a7, #f74f9e, #ff5257)",
    a1: "#8f6bff",
    a2: "#c46cf2",
    a3: "#ff5f9e",
    s1: "#3fd0f0",
    s2: "#5b8cff",
  },
  { id: "blue", label: "Blue", swatch: "#007aff", a1: "#4aa8ff", a2: "#0a84ff", a3: "#3d5afe", ...NEUTRAL },
  { id: "purple", label: "Purple", swatch: "#a550a7", a1: "#c77dff", a2: "#a550a7", a3: "#8a3ab9", ...NEUTRAL },
  { id: "pink", label: "Pink", swatch: "#f74f9e", a1: "#ff8fc6", a2: "#f74f9e", a3: "#e0307f", ...NEUTRAL },
  { id: "red", label: "Red", swatch: "#ff5257", a1: "#ff8a65", a2: "#ff5257", a3: "#e5323f", ...NEUTRAL },
  { id: "orange", label: "Orange", swatch: "#f7821b", a1: "#ffb04d", a2: "#f7821b", a3: "#ea5a0c", ...NEUTRAL },
  { id: "yellow", label: "Yellow", swatch: "#ffc600", a1: "#ffc93c", a2: "#f0a500", a3: "#d4790a", ...NEUTRAL },
  { id: "green", label: "Green", swatch: "#62ba46", a1: "#8fdc6a", a2: "#4fae3f", a3: "#2e8f4a", ...NEUTRAL },
  { id: "graphite", label: "Graphite", swatch: "#8c8c8c", a1: "#a9adbd", a2: "#858a9c", a3: "#5f6476", ...NEUTRAL },
];

export const DEFAULT_ACCENT = "multicolor";

export function accentById(id: string): Accent {
  return ACCENTS.find((a) => a.id === id) ?? ACCENTS[0];
}

function rgb(hex: string): string {
  const n = parseInt(hex.slice(1), 16);
  return `${(n >> 16) & 255} ${(n >> 8) & 255} ${n & 255}`;
}

/** Sets the theme variables on the document root; unknown ids fall back to Multicolor. */
export function applyAccent(id: string): Accent {
  const accent = accentById(id);
  const root = document.documentElement.style;
  for (const key of ["a1", "a2", "a3", "s1", "s2"] as const) {
    root.setProperty(`--${key}`, accent[key]);
    root.setProperty(`--${key}-rgb`, rgb(accent[key]));
  }
  return accent;
}
