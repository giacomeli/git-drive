import { LoaderCircle, RotateCw } from "lucide-react";
import { CATEGORIES, errorText, LEVELS, type Config } from "../lib/api";
import { useConfig } from "../lib/useConfig";
import { useToast } from "../components/ui";

/** What the daemon writes to its log: base level plus a level per area. */
export function RecordingPanel({ daemonRunning }: { daemonRunning: boolean }) {
  const toast = useToast();
  const cfg = useConfig();
  const draft = cfg.draft;

  const apply = async () => {
    try {
      await cfg.save(daemonRunning);
      toast("ok", daemonRunning ? "Log levels saved. The daemon is restarting with them." : "Log levels saved. They apply when the daemon starts.");
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  return (
    <aside className="glass flex w-[19.5rem] flex-none flex-col overflow-hidden">
      <div className="border-b border-line px-5 py-4">
        <h2 className="text-[0.95rem] font-semibold">Recording</h2>
        <p className="mt-0.5 text-xs text-muted">What the daemon writes to the log file</p>
      </div>
      {draft ? (
        <div className="scroll min-h-0 flex-1 px-5 py-4">
          <RecordingFields config={draft} update={cfg.update} />
        </div>
      ) : (
        <p className="px-5 py-4 text-xs text-dim">{cfg.error ?? "Loading"}</p>
      )}
      <div className="flex gap-2 border-t border-line px-5 py-3.5">
        <button className="btn btn-ghost flex-1" disabled={!cfg.dirty || cfg.saving} onClick={cfg.discard}>
          Discard
        </button>
        <button className="btn btn-primary flex-[1.6]" disabled={!cfg.dirty || cfg.saving} onClick={apply}>
          {cfg.saving ? <LoaderCircle className="spin h-3.5 w-3.5" /> : <RotateCw className="h-3.5 w-3.5" />}
          {daemonRunning ? "Save & restart" : "Save"}
        </button>
      </div>
    </aside>
  );
}

/** Base level and per-area levels, editing a config draft owned by the caller. */
export function RecordingFields({
  config,
  update,
}: {
  config: Config;
  update: <K extends keyof Config>(key: K, value: Config[K]) => void;
}) {
  const draft = config;
  const setCategory = (id: string, level: string) => {
    const next = { ...(draft.log_categories ?? {}) };
    if (level === "") delete next[id];
    else next[id] = level;
    update("log_categories", next);
  };
  return (
    <div className="space-y-4">
      <div>
        <p className="label mb-2">Base level</p>
        <select className="field capitalize" value={draft.log_level} onChange={(e) => update("log_level", e.target.value)}>
          {LEVELS.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </select>
      </div>
      <div>
        <p className="label mb-2">Per area</p>
        <ul className="space-y-2.5">
          {CATEGORIES.map((c) => {
            const fallback = c.id === "network" ? "warn" : draft.log_level;
            return (
              <li key={c.id} className="flex items-center gap-3">
                <div className="min-w-0 flex-1">
                  <p className="text-[0.8rem] font-medium">{c.label}</p>
                  <p className="truncate text-[0.66rem] text-dim" title={c.hint}>
                    {c.hint}
                  </p>
                </div>
                <select
                  className="field !h-8 !w-[8.6rem] !rounded-full !pl-3 !text-[0.72rem] capitalize"
                  value={draft.log_categories?.[c.id] ?? ""}
                  onChange={(e) => setCategory(c.id, e.target.value)}
                >
                  <option value="">Default · {fallback}</option>
                  {LEVELS.map((l) => (
                    <option key={l} value={l}>
                      {l}
                    </option>
                  ))}
                </select>
              </li>
            );
          })}
        </ul>
      </div>
      <p className="text-[0.68rem] leading-relaxed text-dim">
        Off leaves an area out of the file. The daemon reads these levels when it starts; RUST_LOG, when set, takes precedence.
      </p>
    </div>
  );
}
