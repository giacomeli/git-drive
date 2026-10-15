import { useCallback, useEffect, useRef, useState } from "react";
import { errorText } from "./api";

/** Calls `load` now and every `intervalMs` while `enabled`; keeps the last good value. */
export function usePoll<T>(load: () => Promise<T>, intervalMs: number, enabled = true) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;

  const refresh = useCallback(async () => {
    try {
      setData(await loadRef.current());
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    const tick = async () => {
      if (alive) await refresh();
    };
    tick();
    const timer = window.setInterval(tick, intervalMs);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [enabled, intervalMs, refresh]);

  return { data, error, refresh };
}

export const isMac = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);
