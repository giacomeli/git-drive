import { useCallback, useEffect, useMemo, useState } from "react";

/** Bounds of a folder's column, in pixels. */
export const COLUMN = { min: 140, max: 640, initial: 256 };
/** Folders whose width is remembered, most recently resized first out. */
const KEEP = 200;
const KEY = "files.columnWidths";

interface Stored {
  /** Width per folder, least recently resized first. */
  dirs: [string, number][];
  /** Folders never resized (Option+drag sets it for all). */
  fallback: number;
}

export const clampTo = (limits: { min: number; max: number }, width: number) =>
  Math.round(Math.min(limits.max, Math.max(limits.min, width)));

function load(): Stored {
  const fresh: Stored = { dirs: [], fallback: COLUMN.initial };
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return fresh;
    const s = JSON.parse(raw) as Partial<Stored>;
    return {
      dirs: Array.isArray(s.dirs) ? s.dirs.filter((d) => typeof d?.[0] === "string" && typeof d?.[1] === "number").slice(-KEEP) : [],
      fallback: typeof s.fallback === "number" ? clampTo(COLUMN, s.fallback) : fresh.fallback,
    };
  } catch {
    // Unreadable or blocked storage: Finder's defaults for this session.
    return fresh;
  }
}

/**
 * The column view's widths, as Finder keeps them: one per folder (the 200
 * most recently resized, kept across sessions) and a default for the others.
 */
export function useColumnWidths() {
  const [stored, setStored] = useState<Stored>(load);
  useEffect(() => {
    try {
      localStorage.setItem(KEY, JSON.stringify(stored));
    } catch {
      // Private mode or blocked storage: the widths last this session only.
    }
  }, [stored]);

  const byDir = useMemo(() => new Map(stored.dirs), [stored.dirs]);
  const widthOf = useCallback((dir: string) => byDir.get(dir) ?? stored.fallback, [byDir, stored.fallback]);
  const setWidth = useCallback(
    (dir: string, width: number) =>
      setStored((s) => ({
        ...s,
        dirs: [...s.dirs.filter(([d]) => d !== dir), [dir, clampTo(COLUMN, width)] as [string, number]].slice(-KEEP),
      })),
    [],
  );
  /** Every column the same width, the new default (Option+drag). */
  const setAll = useCallback((width: number) => setStored((s) => ({ ...s, dirs: [], fallback: clampTo(COLUMN, width) })), []);

  return { widthOf, setWidth, setAll };
}
