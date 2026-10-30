import {
  ArrowDownWideNarrow,
  ChevronLeft,
  ChevronRight,
  Columns3,
  Ellipsis,
  EllipsisVertical,
  Folder,
  LayoutGrid,
  List,
  LoaderCircle,
  Pencil,
  Search,
  X,
} from "lucide-react";
import { useEffect, useRef, useState, type ReactNode, type RefObject } from "react";
import { isMac, WindowButtons } from "./Chrome";
import { t, type Key } from "../i18n";

/** How many folders the location field offers at once. */
const MAX_SUGGESTIONS = 8;
/** The shortcut that turns the path into a field. */
export const EDIT_PATH_KEYS = isMac ? "Cmd+L" : "Ctrl+L";

export type View = "icons" | "list" | "columns";

const VIEWS: { view: View; label: Key; Icon: typeof List }[] = [
  { view: "icons", label: "toolbar.iconView", Icon: LayoutGrid },
  { view: "list", label: "toolbar.listView", Icon: List },
  { view: "columns", label: "toolbar.columnView", Icon: Columns3 },
];

export function IconButton({
  label,
  title,
  onClick,
  disabled = false,
  on = false,
  children,
}: {
  label: string;
  /** The tooltip, when it says more than the label (a shortcut). */
  title?: string;
  /** Gets the button: a menu opens under it. */
  onClick: (button: HTMLButtonElement) => void;
  disabled?: boolean;
  on?: boolean;
  children: ReactNode;
}) {
  return (
    <button
      aria-label={label}
      title={title ?? label}
      onClick={(e) => onClick(e.currentTarget)}
      disabled={disabled}
      className={`grid h-8 w-8 flex-none place-items-center rounded-full border transition-all disabled:opacity-30 ${
        on ? "border-transparent text-paper" : "border-white/10 bg-white/6 text-muted hover:bg-white/12 hover:text-ink"
      }`}
      style={on ? { background: "var(--accent)", boxShadow: "0 6px 16px -6px rgb(var(--a3-rgb) / 0.8)" } : undefined}
    >
      {children}
    </button>
  );
}

export function Toolbar({
  inset,
  canBack,
  canForward,
  onBack,
  onForward,
  crumbs,
  onCrumb,
  editing,
  onEdit,
  onGo,
  folders,
  home,
  view,
  onView,
  onOrganize,
  organizeOn,
  onMore,
  folderView,
  query,
  onQuery,
  searching,
  searchRef,
}: {
  /** Room for the traffic lights (macOS, sidebar collapsed). */
  inset: boolean;
  canBack: boolean;
  canForward: boolean;
  onBack: () => void;
  onForward: () => void;
  crumbs: { label: string; path: string }[];
  onCrumb: (path: string) => void;
  /** The path as a field to type in: its text when it opened (null: the breadcrumbs). */
  editing: string | null;
  onEdit: (open: boolean) => void;
  /** Goes where a typed path leads; the reason when it can't. */
  onGo: (typed: string) => Promise<string | null>;
  /** The folders in `dir`, for the field's suggestions. */
  folders: (dir: string) => Promise<string[]>;
  home: string;
  view: View;
  onView: (view: View) => void;
  /** Opens the Sort By and Group By menu under the button. */
  onOrganize: (button: HTMLButtonElement) => void;
  /** Items are grouped, or sorted other than by name: the button shows it. */
  organizeOn: boolean;
  /** Opens the window's menu (three dots) under the button. */
  onMore: (button: HTMLButtonElement) => void;
  /** A folder is in view (not the Computer view): search and organize apply. */
  folderView: boolean;
  query: string;
  onQuery: (q: string) => void;
  searching: boolean;
  searchRef: RefObject<HTMLInputElement | null>;
}) {
  return (
    <header
      data-tauri-drag-region
      // No right padding: the window menu lines up with the right edge of the folders' container.
      className={`relative z-20 flex h-[3.75rem] flex-none items-center gap-2 ${inset ? "pl-5" : ""}`}
    >
      <div className="flex gap-1.5">
        <IconButton label={t("toolbar.back")} onClick={onBack} disabled={!canBack}>
          <ChevronLeft className="h-4 w-4" />
        </IconButton>
        <IconButton label={t("toolbar.forward")} onClick={onForward} disabled={!canForward}>
          <ChevronRight className="h-4 w-4" />
        </IconButton>
      </div>

      {editing != null ? (
        <PathField initial={editing} home={home} onGo={onGo} folders={folders} onCancel={() => onEdit(false)} />
      ) : (
        <Breadcrumb crumbs={crumbs} onCrumb={onCrumb} onEdit={() => onEdit(true)} />
      )}

      <div className="flex flex-none rounded-full border border-white/10 bg-black/20 p-0.5">
        {VIEWS.map(({ view: v, label, Icon }) => (
          <button
            key={v}
            aria-label={t(label)}
            title={t(label)}
            onClick={() => onView(v)}
            className={`grid h-7 w-9 place-items-center rounded-full transition-all ${v === view ? "text-paper shadow" : "text-dim hover:text-muted"}`}
            style={v === view ? { background: "var(--accent)" } : undefined}
          >
            <Icon className="h-3.5 w-3.5" />
          </button>
        ))}
      </div>
      <IconButton label={t("toolbar.sortGroup")} onClick={onOrganize} disabled={!folderView} on={organizeOn && folderView}>
        <ArrowDownWideNarrow className="h-3.5 w-3.5" />
      </IconButton>

      <label className="relative ml-1 flex-none">
        {searching ? (
          <LoaderCircle className="spin pointer-events-none absolute left-3.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dim" />
        ) : (
          <Search className="pointer-events-none absolute left-3.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dim" />
        )}
        <input
          ref={searchRef}
          className="field !h-9 w-56 !rounded-full !pl-9 !pr-8 disabled:opacity-40"
          disabled={!folderView}
          placeholder={t("toolbar.search")}
          value={query}
          spellCheck={false}
          onChange={(e) => onQuery(e.target.value)}
        />
        {query && (
          <button
            aria-label={t("toolbar.clearSearch")}
            onClick={() => onQuery("")}
            className="absolute right-2.5 top-1/2 -translate-y-1/2 text-dim hover:text-ink"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        )}
      </label>
      {/* No ring: it sits at the window's edge, its icon close to the container's border. */}
      <button
        aria-label={t("toolbar.more")}
        title={t("toolbar.moreOptions")}
        onClick={(e) => onMore(e.currentTarget)}
        className={`${isMac ? "-mr-1.5" : ""} grid h-8 w-7 flex-none place-items-center rounded-lg text-muted transition-colors hover:bg-white/8 hover:text-ink`}
      >
        <EllipsisVertical className="h-4 w-4" />
      </button>
      <WindowButtons />
    </header>
  );
}

/** Path segments; a long path keeps the first and the last two, the middle
 * collapsed into a button that goes to the deepest hidden folder. The folder
 * in view never shrinks; the ones before it give way first. */
function Breadcrumb({ crumbs, onCrumb, onEdit }: { crumbs: { label: string; path: string }[]; onCrumb: (path: string) => void; onEdit: () => void }) {
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
                aria-label={t("toolbar.enclosing")}
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
      <button
        aria-label={t("toolbar.typePath")}
        title={`${t("toolbar.typePath")} (${EDIT_PATH_KEYS})`}
        onClick={onEdit}
        className="ml-1 grid h-7 w-7 flex-none place-items-center rounded-full text-dim transition-colors hover:bg-white/8 hover:text-ink"
      >
        <Pencil className="h-3.5 w-3.5" />
      </button>
    </nav>
  );
}

/** `~` and `~/...` as the home folder; the folder a typed parent names. */
function expandDir(typed: string, home: string): string {
  const full = typed === "~" || typed.startsWith("~/") ? home + typed.slice(1) : typed;
  return full.length > 1 ? full.replace(/\/+$/, "") : full;
}

/** The longest start every one of `words` shares (case kept from the first). */
function commonStart(words: string[]): string {
  if (words.length === 0) return "";
  let end = words[0].length;
  for (const w of words.slice(1)) {
    let i = 0;
    while (i < end && i < w.length && w[i].toLowerCase() === words[0][i].toLowerCase()) i++;
    end = i;
  }
  return words[0].slice(0, end);
}

/**
 * The path as a text field (Cmd+L, or the button after the breadcrumbs):
 * `~` is the home folder, a relative path starts at the folder in view.
 * Folders of the typed parent are suggested as the last name is typed; Tab
 * completes, the arrows pick, Return goes (a file: its folder, with it
 * selected), Esc or a click elsewhere brings the breadcrumbs back.
 */
function PathField({
  initial,
  home,
  onGo,
  folders,
  onCancel,
}: {
  initial: string;
  home: string;
  onGo: (typed: string) => Promise<string | null>;
  folders: (dir: string) => Promise<string[]>;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initial);
  const [error, setError] = useState<string | null>(null);
  const [hits, setHits] = useState<string[]>([]);
  const [pick, setPick] = useState(-1);
  const input = useRef<HTMLInputElement | null>(null);
  const cache = useRef(new Map<string, Promise<string[]>>());

  useEffect(() => {
    const el = input.current;
    if (!el) return;
    el.focus();
    // A whole path opens selected (typing replaces it); "/" or "~" typed into the view goes on from there.
    if (initial.length > 1) el.select();
    else el.setSelectionRange(initial.length, initial.length);
  }, [initial]);

  // The folders of the typed parent whose names start with what follows the last slash.
  useEffect(() => {
    let alive = true;
    const cut = value.lastIndexOf("/");
    if (cut < 0) {
      setHits([]);
      return;
    }
    const parent = value.slice(0, cut + 1);
    const prefix = value.slice(cut + 1).toLowerCase();
    const dir = expandDir(parent, home);
    let names = cache.current.get(dir);
    if (!names) {
      names = folders(dir).catch(() => []);
      cache.current.set(dir, names);
    }
    void names.then((all) => {
      if (!alive) return;
      const list = all
        .filter((n) => n.toLowerCase().startsWith(prefix) && (prefix.startsWith(".") || !n.startsWith(".")))
        .sort((a, b) => a.localeCompare(b, undefined, { numeric: true, sensitivity: "base" }))
        .slice(0, MAX_SUGGESTIONS)
        .map((n) => parent + n);
      // Nothing to offer when the one match is what is typed.
      setHits(list.length === 1 && list[0] === value ? [] : list);
      setPick(-1);
    });
    return () => {
      alive = false;
    };
  }, [value, home, folders]);

  const go = async (typed: string) => {
    const why = await onGo(typed);
    if (why) setError(why);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    // The field's own keys never reach the view (its arrows, Return, Esc).
    e.stopPropagation();
    if (e.key === "Escape") {
      e.preventDefault();
      onCancel();
    } else if (e.key === "Enter") {
      e.preventDefault();
      void go(pick >= 0 ? hits[pick] : value);
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (hits.length === 0) return;
      const step = e.key === "ArrowDown" ? 1 : -1;
      // From the field (-1) through the suggestions and back.
      setPick((p) => {
        const next = p + step;
        return next < -1 ? hits.length - 1 : next >= hits.length ? -1 : next;
      });
    } else if (e.key === "Tab") {
      e.preventDefault();
      if (hits.length === 0) return;
      const chosen = pick >= 0 ? hits[pick] : hits.length === 1 ? hits[0] : null;
      if (chosen) setValue(`${chosen}/`);
      else {
        const shared = commonStart(hits);
        if (shared.length > value.length) setValue(shared);
      }
      setError(null);
    }
  };

  return (
    <div className="relative mx-2 min-w-0 flex-1">
      <input
        ref={input}
        aria-label={t("toolbar.path")}
        className={`field !h-8 w-full !rounded-full !px-4 font-mono !text-[0.76rem] ${error ? "!ring-1 !ring-[rgb(255_92_122/0.7)]" : ""}`}
        value={value}
        spellCheck={false}
        autoCorrect="off"
        autoCapitalize="off"
        placeholder={t("toolbar.pathPlaceholder")}
        onChange={(e) => {
          setValue(e.target.value);
          setError(null);
        }}
        onKeyDown={onKeyDown}
        onBlur={onCancel}
      />
      {(error || hits.length > 0) && (
        <div className="float-panel absolute left-0 right-0 top-full z-30 mt-1.5 py-1.5" role="listbox" aria-label={t("toolbar.folders")}>
          {error ? (
            <p className="px-3.5 py-1.5 text-[0.74rem] text-coral">{error}</p>
          ) : (
            hits.map((h, i) => {
              const cut = h.lastIndexOf("/");
              return (
                <div
                  key={h}
                  role="option"
                  aria-selected={i === pick}
                  // A press on a suggestion must not blur the field first.
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={() => void go(h)}
                  onMouseEnter={() => setPick(i)}
                  className={`mx-1.5 flex cursor-default items-center gap-2 rounded-lg px-2.5 py-1.5 text-[0.76rem] ${i === pick ? "bg-white/10 text-ink" : "text-muted"}`}
                >
                  <Folder className="h-3.5 w-3.5 flex-none accent-fg" />
                  <span className="truncate">
                    <span className="text-dim">{h.slice(0, cut + 1)}</span>
                    <span className={i === pick ? "text-ink" : "text-ink/90"}>{h.slice(cut + 1)}</span>
                  </span>
                </div>
              );
            })
          )}
        </div>
      )}
    </div>
  );
}
