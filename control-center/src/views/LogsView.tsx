import { ArrowDown, FolderOpen, Pause, Play, Search, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { api, CATEGORIES, errorText, type LogEntry } from "../lib/api";
import { tildify, tildifyAll } from "../lib/format";
import { usePoll } from "../lib/hooks";
import { PageHeader } from "../components/Shell";
import { useToast } from "../components/ui";
import { RecordingPanel } from "./RecordingPanel";

const MAX_ENTRIES = 1500;

const LEVEL_STYLE: Record<string, { color: string; label: string }> = {
  ERROR: { color: "#ff5c7a", label: "Error" },
  WARN: { color: "#ffb547", label: "Warn" },
  INFO: { color: "#3fd0f0", label: "Info" },
  DEBUG: { color: "#a58bff", label: "Debug" },
  TRACE: { color: "#6f7499", label: "Trace" },
};
const LEVEL_ORDER = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"];
const CATEGORY_LABEL = Object.fromEntries(CATEGORIES.map((c) => [c.id, c.label]));

export function LogsView({ daemonRunning }: { daemonRunning: boolean }) {
  const toast = useToast();
  const [paused, setPaused] = useState(false);
  const [query, setQuery] = useState("");
  const [levels, setLevels] = useState<Set<string>>(new Set(LEVEL_ORDER));
  const [category, setCategory] = useState<string | null>(null);
  const [follow, setFollow] = useState(true);
  const listRef = useRef<HTMLDivElement>(null);
  const { data, error } = usePoll(() => api.logs(MAX_ENTRIES), 2000, !paused);

  const entries = data?.entries ?? [];
  const counts = useMemo(() => {
    const c: Record<string, number> = {};
    for (const e of entries) c[e.level] = (c[e.level] ?? 0) + 1;
    return c;
  }, [entries]);

  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    return entries.filter(
      (e) =>
        levels.has(e.level) &&
        (category == null || e.category === category) &&
        (q === "" || e.message.toLowerCase().includes(q) || e.module.toLowerCase().includes(q)),
    );
  }, [entries, levels, category, query]);

  // Keep the newest line in view while following.
  useEffect(() => {
    const el = listRef.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [visible, follow]);

  const onScroll = () => {
    const el = listRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    if (atBottom !== follow) setFollow(atBottom);
  };

  const toggleLevel = (level: string) =>
    setLevels((prev) => {
      const next = new Set(prev);
      if (next.has(level)) next.delete(level);
      else next.add(level);
      return next.size === 0 ? new Set(LEVEL_ORDER) : next;
    });

  const filtered = query !== "" || category != null || levels.size !== LEVEL_ORDER.length;
  const logDir = data?.file ? data.file.replace(/\/[^/]+$/, "") : null;

  return (
    <div className="fade-in flex h-full min-h-0 flex-col">
      <PageHeader
        title="Logs"
        subtitle={data?.file ? tildify(data.file) : "Session log of the daemon"}
        actions={
          <>
            {logDir && (
              <button className="btn btn-ghost" onClick={() => api.openPath(logDir).catch((e) => toast("error", errorText(e)))}>
                <FolderOpen className="h-4 w-4" />
                Log folder
              </button>
            )}
            <button className={`btn ${paused ? "btn-primary" : "btn-ghost"}`} onClick={() => setPaused((p) => !p)}>
              {paused ? <Play className="h-3.5 w-3.5" fill="currentColor" /> : <Pause className="h-3.5 w-3.5" fill="currentColor" />}
              {paused ? "Resume" : "Pause"}
            </button>
          </>
        }
      />

      <div className="flex min-h-0 flex-1 gap-5">
        <section className="glass flex min-w-0 flex-1 flex-col overflow-hidden">
          <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-3">
            <label className="relative mr-1 flex-none">
              <Search className="pointer-events-none absolute left-3.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dim" />
              <input
                className="field !h-9 w-52 !rounded-full !pl-9"
                placeholder="Search messages"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            {LEVEL_ORDER.map((level) => (
              <button key={level} className={`chip ${levels.has(level) ? "on" : ""}`} onClick={() => toggleLevel(level)}>
                <span className="dot" style={{ background: LEVEL_STYLE[level].color }} />
                {LEVEL_STYLE[level].label}
                <span className="tabular-nums text-dim">{counts[level] ?? 0}</span>
              </button>
            ))}
          </div>
          <div className="flex flex-wrap gap-1.5 border-b border-line px-4 py-2.5">
            <button className={`chip ${category == null ? "on" : ""}`} onClick={() => setCategory(null)}>
              All areas
            </button>
            {CATEGORIES.map((c) => (
              <button key={c.id} title={c.hint} className={`chip ${category === c.id ? "on" : ""}`} onClick={() => setCategory(category === c.id ? null : c.id)}>
                {c.label}
              </button>
            ))}
            {filtered && (
              <button
                className="chip ml-auto"
                onClick={() => {
                  setQuery("");
                  setCategory(null);
                  setLevels(new Set(LEVEL_ORDER));
                }}
              >
                <X className="h-3 w-3" />
                Clear
              </button>
            )}
          </div>

          <div ref={listRef} onScroll={onScroll} className="scroll selectable relative min-h-0 flex-1 px-2 py-2 font-mono text-[0.72rem] leading-[1.55]">
            {visible.map((e, i) => (
              <LogRow key={`${e.time}-${i}`} entry={e} />
            ))}
            {visible.length === 0 && (
              <div className="grid h-full place-items-center font-sans text-xs text-dim">
                {error
                  ? error
                  : entries.length === 0
                    ? daemonRunning
                      ? "The log is empty."
                      : "No log yet: the daemon has not run on this machine."
                    : "No line matches the filters."}
              </div>
            )}
          </div>
          <footer className="flex items-center justify-between border-t border-line px-4 py-2 text-[0.68rem] text-dim">
            <span>
              {visible.length} of {entries.length} lines
              {paused && " · paused"}
            </span>
            {!follow && (
              <button className="accent-fg inline-flex items-center gap-1.5 hover:text-ink" onClick={() => setFollow(true)}>
                <ArrowDown className="h-3 w-3" />
                Jump to latest
              </button>
            )}
          </footer>
        </section>

        <RecordingPanel daemonRunning={daemonRunning} />
      </div>
    </div>
  );
}

function LogRow({ entry }: { entry: LogEntry }) {
  const style = LEVEL_STYLE[entry.level] ?? LEVEL_STYLE.TRACE;
  const urgent = entry.level === "ERROR" || entry.level === "WARN";
  return (
    <div
      className="group flex gap-3 rounded-lg px-2.5 py-1 hover:bg-white/5"
      style={urgent ? { background: `${style.color}0d` } : undefined}
    >
      <span className="flex-none text-dim">{entry.time.slice(11)}</span>
      <span className="w-12 flex-none font-semibold" style={{ color: style.color }}>
        {entry.level}
      </span>
      <span className="w-20 flex-none truncate text-muted" title={entry.module}>
        {CATEGORY_LABEL[entry.category] ?? entry.category}
      </span>
      <span className="min-w-0 flex-1 whitespace-pre-wrap break-words text-ink/90">{tildifyAll(entry.message)}</span>
    </div>
  );
}
