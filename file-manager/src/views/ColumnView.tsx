import { ChevronRight, LoaderCircle } from "lucide-react";
import { useEffect, useRef } from "react";
import type { Entry } from "../lib/api";
import type { DirState } from "../lib/useDir";
import { iconFor } from "../lib/kinds";
import { keepVisible } from "../lib/scroll";
import { PreviewPane } from "./Preview";

/**
 * Finder's column view: one column per folder from the anchor place down to
 * the location; a selected file adds its preview as the last column.
 */
export function ColumnView({
  columns,
  dirs,
  selected,
  preview,
  onSelect,
  onOpen,
  platform,
}: {
  columns: string[];
  dirs: Record<string, DirState>;
  selected: string | null;
  preview: Entry | null;
  onSelect: (entry: Entry, columnDir: string) => void;
  onOpen: (entry: Entry) => void;
  platform: "macOS" | "Linux";
}) {
  const scroller = useRef<HTMLDivElement | null>(null);
  // Keep the deepest column in view, also once the columns finish loading.
  const loaded = columns.filter((c) => dirs[c]?.listing).length;
  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const target = el.scrollWidth - el.clientWidth;
    el.scrollLeft = target;
  }, [columns.join("\n"), loaded, preview?.path]);

  return (
    <div ref={scroller} className="scroll flex min-h-0 flex-1 overflow-x-auto overflow-y-hidden">
      {columns.map((dir, i) => {
        const state = dirs[dir];
        const next = columns[i + 1];
        return (
          <div key={dir} className="flex w-64 flex-none flex-col border-r border-line">
            <div className="scroll min-h-0 flex-1 px-1.5 py-1.5">
              {state?.listing?.entries.map((e) => {
                const { Icon, color } = iconFor(e);
                const focused = e.path === selected;
                const onPath = !focused && e.path === next;
                return (
                  <ColumnRow
                    key={e.path}
                    entry={e}
                    focused={focused}
                    onPath={onPath}
                    icon={<Icon className="h-4 w-4 flex-none" style={{ color }} strokeWidth={1.9} />}
                    onSelect={() => onSelect(e, dir)}
                    onOpen={() => onOpen(e)}
                  />
                );
              })}
              {state?.loading && !state.listing && (
                <div className="grid h-20 place-items-center text-dim">
                  <LoaderCircle className="spin h-4 w-4" />
                </div>
              )}
              {state?.error && <p className="px-3 py-3 text-[0.72rem] text-coral">{state.error}</p>}
              {state?.listing && state.listing.entries.length === 0 && (
                <p className="px-3 py-3 text-[0.72rem] text-dim">Empty folder</p>
              )}
            </div>
          </div>
        );
      })}
      {preview && !preview.is_dir && (
        <div className="flex w-[22rem] flex-none flex-col">
          <PreviewPane entry={preview} platform={platform} bare />
        </div>
      )}
    </div>
  );
}

function ColumnRow({
  entry,
  focused,
  onPath,
  icon,
  onSelect,
  onOpen,
}: {
  entry: Entry;
  focused: boolean;
  onPath: boolean;
  icon: React.ReactNode;
  onSelect: () => void;
  onOpen: () => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (focused || onPath) keepVisible(ref.current);
  }, [focused, onPath]);
  return (
    <div
      ref={ref}
      onMouseDown={onSelect}
      onDoubleClick={onOpen}
      className={`flex h-8 cursor-default items-center gap-2.5 rounded-lg px-2.5 text-[0.8rem] ${
        focused ? "text-white" : onPath ? "bg-white/10 text-ink" : "hover:bg-white/5"
      } ${entry.hidden ? "opacity-60" : ""}`}
      style={focused ? { background: "rgb(var(--a1-rgb) / 0.32)", boxShadow: "inset 0 0 0 1px rgb(var(--a1-rgb) / 0.4)" } : undefined}
    >
      {icon}
      <span className="min-w-0 flex-1 truncate">{entry.name}</span>
      {entry.is_dir && <ChevronRight className={`h-3.5 w-3.5 flex-none ${focused ? "text-white/80" : "text-dim"}`} />}
    </div>
  );
}
