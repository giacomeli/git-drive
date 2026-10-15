import { useCallback, useEffect, useRef, useState } from "react";
import { api, errorText, type Listing } from "./api";

export interface DirState {
  listing: Listing | null;
  error: string | null;
  loading: boolean;
}

/**
 * Listings of several folders at once (one per Finder column), cached by
 * path; `refresh` re-reads every folder in view (window focus, Cmd+R).
 */
export function useDirs(paths: string[], showHidden: boolean) {
  const [dirs, setDirs] = useState<Record<string, DirState>>({});
  const generation = useRef(0);
  const key = paths.join("\n") + `|${showHidden}`;

  const load = useCallback(
    async (wanted: string[], keep: boolean) => {
      const gen = ++generation.current;
      setDirs((prev) => {
        const next: Record<string, DirState> = {};
        for (const p of wanted) next[p] = keep && prev[p] ? { ...prev[p], loading: true } : { listing: null, error: null, loading: true };
        return next;
      });
      await Promise.all(
        wanted.map(async (p) => {
          try {
            const listing = await api.listDir(p, showHidden);
            if (gen === generation.current) setDirs((prev) => ({ ...prev, [p]: { listing, error: null, loading: false } }));
          } catch (e) {
            if (gen === generation.current) setDirs((prev) => ({ ...prev, [p]: { listing: null, error: errorText(e), loading: false } }));
          }
        }),
      );
    },
    [showHidden],
  );

  useEffect(() => {
    load(paths, true);
    // `key` captures the paths and the hidden flag.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  const refresh = useCallback(() => load(paths, true), [load, paths]);
  return { dirs, refresh };
}
