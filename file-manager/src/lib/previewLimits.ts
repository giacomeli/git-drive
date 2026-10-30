import { useSyncExternalStore } from "react";
import type { PreviewKind } from "./api";

// How large a file the previews process (render Markdown, format JSON, draw a
// 3D model): one limit for the preview panel, a larger one for Quick Look,
// which the user opens on purpose. Settings changes them; the previews read
// them through `usePreviewLimit`.

export type PreviewPlace = "panel" | "quickLook";

/** The choices, in bytes (decimal, as sizes are shown). */
export const LIMIT_CHOICES: Record<PreviewPlace, readonly number[]> = {
  panel: [250_000, 1_000_000, 5_000_000, 20_000_000],
  quickLook: [1_000_000, 5_000_000, 20_000_000, 100_000_000],
};
export const LIMIT_DEFAULTS: Record<PreviewPlace, number> = { panel: 1_000_000, quickLook: 20_000_000 };

const KEYS: Record<PreviewPlace, string> = { panel: "files.previewLimitPanel", quickLook: "files.previewLimitQuickLook" };

/** A stored value that is one of the choices, else the default. */
export function validLimit(place: PreviewPlace, raw: unknown): number {
  return typeof raw === "number" && LIMIT_CHOICES[place].includes(raw) ? raw : LIMIT_DEFAULTS[place];
}

function load(place: PreviewPlace): number {
  try {
    const raw = localStorage.getItem(KEYS[place]);
    return validLimit(place, raw == null ? null : JSON.parse(raw));
  } catch {
    return LIMIT_DEFAULTS[place];
  }
}

const limits: Record<PreviewPlace, number> = { panel: load("panel"), quickLook: load("quickLook") };
const listeners = new Set<() => void>();
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

export const previewLimit = (place: PreviewPlace) => limits[place];

export function setPreviewLimit(place: PreviewPlace, bytes: number) {
  limits[place] = validLimit(place, bytes);
  try {
    localStorage.setItem(KEYS[place], JSON.stringify(limits[place]));
  } catch {
    // Private mode or blocked storage: the setting lasts this session only.
  }
  listeners.forEach((l) => l());
}

export function usePreviewLimit(place: PreviewPlace): number {
  return useSyncExternalStore(subscribe, () => limits[place]);
}

/**
 * How a file of `size` bytes and preview `kind` shows under `limit`:
 * - "render": as its kind draws it (Markdown rendered, JSON formatted, the 3D
 *   model drawn; images, video, audio and PDF are the webview's own and never limited);
 * - "text": Markdown and JSON past the limit, as plain text (the bounded read);
 * - "details": a 3D model past the limit, its details and a button to draw it anyway.
 */
export function howToShow(kind: PreviewKind, size: number, limit: number): "render" | "text" | "details" {
  const within = size <= limit;
  switch (kind) {
    case "markdown":
    case "json":
      return within ? "render" : "text";
    case "model":
      return within ? "render" : "details";
    default:
      return "render";
  }
}
