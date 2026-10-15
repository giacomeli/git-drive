import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorText, type Config, type ConfigView } from "./api";

/**
 * Draft of the config file: loaded once, edited locally, saved atomically by
 * the backend. The daemon reads the config only at boot, so applying a
 * change means saving and restarting the daemon.
 */
export function useConfig() {
  const [view, setView] = useState<ConfigView | null>(null);
  const [draft, setDraft] = useState<Config | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    try {
      const v = await api.getConfig();
      setView(v);
      setDraft(structuredClone(v.config));
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const dirty = useMemo(() => {
    if (!view || !draft) return false;
    return JSON.stringify(normalize(view.config)) !== JSON.stringify(normalize(draft));
  }, [view, draft]);

  const update = useCallback(<K extends keyof Config>(key: K, value: Config[K]) => {
    setDraft((d) => (d ? { ...d, [key]: value } : d));
  }, []);

  const discard = useCallback(() => {
    if (view) setDraft(structuredClone(view.config));
  }, [view]);

  /** Saves the draft; with `restart`, restarts the daemon so it boots with it. */
  const save = useCallback(
    async (restart: boolean) => {
      if (!draft) return;
      setSaving(true);
      try {
        const clean = normalize(draft);
        await api.saveConfig(clean);
        setView((v) => (v ? { ...v, exists: true, config: clean } : v));
        setDraft(structuredClone(clean));
        if (restart) await api.daemon("restart");
      } finally {
        setSaving(false);
      }
    },
    [draft],
  );

  return { view, draft, error, saving, dirty, update, discard, save, reload: load };
}

/** Empty optional strings become unset; empty category maps disappear. */
function normalize(c: Config): Config {
  const out: Config = { ...c };
  for (const key of [
    "encryption_key_file",
    "user_name",
    "user_email",
    "remote_token",
    "p2p_key_file",
    "p2p_device_name",
    "p2p_relay_url",
  ] as const) {
    const v = out[key];
    if (v == null || (typeof v === "string" && v.trim() === "")) delete out[key];
    else out[key] = (v as string).trim();
  }
  const cats = Object.fromEntries(Object.entries(out.log_categories ?? {}).filter(([, v]) => v));
  if (Object.keys(cats).length > 0) out.log_categories = cats;
  else delete out.log_categories;
  return out;
}
