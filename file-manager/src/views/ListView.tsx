import { ChevronDown, ChevronUp } from "lucide-react";
import { useEffect, useRef } from "react";
import type { Entry } from "../lib/api";
import { dateLabel, iconFor, kindLabel } from "../lib/kinds";
import { keepVisible } from "../lib/scroll";
import { bytesText } from "../ui/format";

export type SortKey = "name" | "modified" | "size" | "kind";
export interface Sort {
  key: SortKey;
  asc: boolean;
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** Folders stay on top; then the chosen column, name as the tie-breaker. */
export function sortEntries(entries: Entry[], sort: Sort): Entry[] {
  const dir = sort.asc ? 1 : -1;
  const by = (a: Entry, b: Entry): number => {
    switch (sort.key) {
      case "modified":
        return (a.modified ?? 0) - (b.modified ?? 0);
      case "size":
        return a.size - b.size;
      case "kind":
        return collator.compare(kindLabel(a), kindLabel(b));
      default:
        return collator.compare(a.name, b.name);
    }
  };
  return [...entries].sort(
    (a, b) => Number(b.is_dir) - Number(a.is_dir) || dir * by(a, b) || collator.compare(a.name, b.name),
  );
}

const COLUMNS: { key: SortKey; label: string; className: string }[] = [
  { key: "name", label: "Name", className: "min-w-0 flex-1" },
  { key: "modified", label: "Date Modified", className: "w-36 flex-none" },
  { key: "size", label: "Size", className: "w-[4.5rem] flex-none text-right" },
  { key: "kind", label: "Kind", className: "w-32 flex-none" },
];

export function ListView({
  entries,
  sort,
  onSort,
  selected,
  onSelect,
  onOpen,
  whereRoot,
  empty,
}: {
  entries: Entry[];
  sort: Sort;
  onSort: (sort: Sort) => void;
  selected: string | null;
  onSelect: (entry: Entry) => void;
  onOpen: (entry: Entry) => void;
  /** Search results: show where each entry lives, relative to this folder. */
  whereRoot?: string;
  empty: string;
}) {
  const selectedRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    keepVisible(selectedRef.current);
  }, [selected]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex h-9 flex-none items-center gap-3 border-b border-line px-5 text-[0.7rem] font-semibold text-dim">
        {COLUMNS.filter((c) => !(whereRoot && c.key === "kind")).map((c) => {
          const active = sort.key === c.key;
          const Arrow = sort.asc ? ChevronUp : ChevronDown;
          return (
            <button
              key={c.key}
              onClick={() => onSort({ key: c.key, asc: active ? !sort.asc : true })}
              className={`flex items-center gap-1 ${c.className} ${c.key === "size" ? "justify-end" : ""} ${active ? "text-ink" : "hover:text-muted"}`}
            >
              {c.label}
              {active && <Arrow className="h-3 w-3" />}
            </button>
          );
        })}
        {whereRoot && <span className="w-40 flex-none">Where</span>}
      </div>
      <div className="scroll min-h-0 flex-1 px-2 py-1.5">
        {entries.map((e) => {
          const { Icon, color } = iconFor(e);
          const on = e.path === selected;
          return (
            <div
              key={e.path}
              ref={on ? selectedRef : undefined}
              onMouseDown={() => onSelect(e)}
              onDoubleClick={() => onOpen(e)}
              className={`flex h-8 cursor-default items-center gap-3 rounded-lg px-3 text-[0.8rem] ${
                on ? "text-white" : "odd:bg-white/[0.025] hover:bg-white/5"
              } ${e.hidden ? "opacity-60" : ""}`}
              style={on ? { background: "rgb(var(--a1-rgb) / 0.32)", boxShadow: "inset 0 0 0 1px rgb(var(--a1-rgb) / 0.4)" } : undefined}
            >
              <span className="flex min-w-0 flex-1 items-center gap-2.5">
                <Icon className="h-4 w-4 flex-none" style={{ color }} strokeWidth={1.9} />
                <span className="truncate">{e.name}</span>
              </span>
              <span className={`w-36 flex-none truncate ${on ? "text-white/80" : "text-muted"}`}>{dateLabel(e.modified)}</span>
              <span className={`w-[4.5rem] flex-none text-right tabular-nums ${on ? "text-white/80" : "text-muted"}`}>
                {e.is_dir ? "--" : bytesText(e.size)}
              </span>
              {!whereRoot && <span className={`w-32 flex-none truncate ${on ? "text-white/80" : "text-muted"}`}>{kindLabel(e)}</span>}
              {whereRoot && (
                <span className={`w-40 flex-none truncate ${on ? "text-white/80" : "text-dim"}`} title={e.path}>
                  {relativeParent(e.path, whereRoot)}
                </span>
              )}
            </div>
          );
        })}
        {entries.length === 0 && <div className="grid h-full place-items-center text-xs text-dim">{empty}</div>}
      </div>
    </div>
  );
}

function relativeParent(path: string, root: string): string {
  const parent = path.slice(0, path.lastIndexOf("/")) || "/";
  if (parent === root) return "here";
  return parent.startsWith(root + "/") ? parent.slice(root.length + 1) : parent;
}
