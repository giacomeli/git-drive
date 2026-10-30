import { useEffect, useState } from "react";

/** A per-window convenience kept in localStorage (it may be unavailable). */
export function useStored<T>(key: string, initial: T): [T, (v: T | ((prev: T) => T)) => void] {
  const [value, setValue] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(`files.${key}`);
      return raw == null ? initial : (JSON.parse(raw) as T);
    } catch {
      return initial;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(`files.${key}`, JSON.stringify(value));
    } catch {
      // Private mode or blocked storage: the setting lasts this session only.
    }
  }, [key, value]);
  return [value, setValue];
}
