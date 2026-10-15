import { ArrowUp, ChevronLeft, ChevronRight, Columns3, Ellipsis, Eye, EyeOff, List, LoaderCircle, PanelRight, Search, X } from "lucide-react";
import type { ReactNode, RefObject } from "react";
import { WindowButtons } from "./Chrome";

export type View = "list" | "columns";

function IconButton({
  label,
  onClick,
  disabled = false,
  on = false,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  on?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
      className={`grid h-8 w-8 flex-none place-items-center rounded-full border transition-all disabled:opacity-30 ${
        on ? "border-transparent text-white" : "border-white/10 bg-white/6 text-muted hover:bg-white/12 hover:text-ink"
      }`}
      style={on ? { background: "var(--accent)", boxShadow: "0 6px 16px -6px rgb(var(--a3-rgb) / 0.8)" } : undefined}
    >
      {children}
    </button>
  );
}

export function Toolbar({
  canBack,
  canForward,
  canUp,
  onBack,
  onForward,
  onUp,
  crumbs,
  onCrumb,
  view,
  onView,
  showHidden,
  onHidden,
  showPreview,
  onPreview,
  query,
  onQuery,
  searching,
  searchRef,
}: {
  canBack: boolean;
  canForward: boolean;
  canUp: boolean;
  onBack: () => void;
  onForward: () => void;
  onUp: () => void;
  crumbs: { label: string; path: string }[];
  onCrumb: (path: string) => void;
  view: View;
  onView: (view: View) => void;
  showHidden: boolean;
  onHidden: () => void;
  showPreview: boolean;
  onPreview: () => void;
  query: string;
  onQuery: (q: string) => void;
  searching: boolean;
  searchRef: RefObject<HTMLInputElement | null>;
}) {
  return (
    <header data-tauri-drag-region className="relative z-20 flex h-[3.75rem] flex-none items-center gap-2 pr-4">
      <div className="flex gap-1.5">
        <IconButton label="Back" onClick={onBack} disabled={!canBack}>
          <ChevronLeft className="h-4 w-4" />
        </IconButton>
        <IconButton label="Forward" onClick={onForward} disabled={!canForward}>
          <ChevronRight className="h-4 w-4" />
        </IconButton>
        <IconButton label="Enclosing folder" onClick={onUp} disabled={!canUp}>
          <ArrowUp className="h-4 w-4" />
        </IconButton>
      </div>

      <Breadcrumb crumbs={crumbs} onCrumb={onCrumb} />

      <div className="flex flex-none rounded-full border border-white/10 bg-black/20 p-0.5">
        {(["list", "columns"] as const).map((v) => {
          const Icon = v === "list" ? List : Columns3;
          return (
            <button
              key={v}
              aria-label={v === "list" ? "List view" : "Column view"}
              title={v === "list" ? "List view" : "Column view"}
              onClick={() => onView(v)}
              className={`grid h-7 w-9 place-items-center rounded-full transition-all ${v === view ? "text-white shadow" : "text-dim hover:text-muted"}`}
              style={v === view ? { background: "var(--accent)" } : undefined}
            >
              <Icon className="h-3.5 w-3.5" />
            </button>
          );
        })}
      </div>
      <IconButton label={showHidden ? "Hide hidden files" : "Show hidden files"} onClick={onHidden} on={showHidden}>
        {showHidden ? <Eye className="h-3.5 w-3.5" /> : <EyeOff className="h-3.5 w-3.5" />}
      </IconButton>
      {view === "list" && (
        <IconButton label={showPreview ? "Hide preview" : "Show preview"} onClick={onPreview} on={showPreview}>
          <PanelRight className="h-3.5 w-3.5" />
        </IconButton>
      )}

      <label className="relative ml-1 flex-none">
        {searching ? (
          <LoaderCircle className="spin pointer-events-none absolute left-3.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dim" />
        ) : (
          <Search className="pointer-events-none absolute left-3.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dim" />
        )}
        <input
          ref={searchRef}
          className="field !h-9 w-56 !rounded-full !pl-9 !pr-8"
          placeholder="Search"
          value={query}
          spellCheck={false}
          onChange={(e) => onQuery(e.target.value)}
        />
        {query && (
          <button
            aria-label="Clear search"
            onClick={() => onQuery("")}
            className="absolute right-2.5 top-1/2 -translate-y-1/2 text-dim hover:text-ink"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        )}
      </label>
      <WindowButtons />
    </header>
  );
}

/** Path segments; a long path keeps the first and the last two, the middle
 * collapsed into a button that goes to the deepest hidden folder. The folder
 * in view never shrinks; the ones before it give way first. */
function Breadcrumb({ crumbs, onCrumb }: { crumbs: { label: string; path: string }[]; onCrumb: (path: string) => void }) {
  const collapsed = crumbs.length > 4;
  const hidden = collapsed ? crumbs.slice(1, -2) : [];
  const shown = collapsed ? [crumbs[0], null, ...crumbs.slice(-2)] : crumbs;
  return (
    <nav data-tauri-drag-region className="mx-2 flex min-w-0 flex-1 items-center gap-0.5 overflow-hidden">
      {shown.map((c, i) => {
        const last = i === shown.length - 1;
        return (
          <span key={c ? c.path : "gap"} className={`flex items-center gap-0.5 ${last || !c ? "flex-none" : "min-w-0"}`}>
            {i > 0 && <ChevronRight className="h-3.5 w-3.5 flex-none text-dim" />}
            {c ? (
              <button
                onClick={() => onCrumb(c.path)}
                title={c.path}
                className={`truncate rounded-lg px-2 py-1 text-[0.82rem] transition-colors hover:bg-white/8 ${
                  last ? "max-w-[18rem] font-semibold text-ink" : "max-w-[9rem] text-muted"
                }`}
              >
                {c.label}
              </button>
            ) : (
              <button
                aria-label="Enclosing folders"
                title={hidden.map((h) => h.label).join(" / ")}
                onClick={() => onCrumb(hidden[hidden.length - 1].path)}
                className="grid h-7 w-8 place-items-center rounded-lg text-muted transition-colors hover:bg-white/8 hover:text-ink"
              >
                <Ellipsis className="h-4 w-4" />
              </button>
            )}
          </span>
        );
      })}
    </nav>
  );
}
