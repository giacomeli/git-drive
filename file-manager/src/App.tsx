import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, baseName, errorText, parentOf, type Boot, type Entry, type Space } from "./lib/api";
import { anchorOf, columnsFor, crumbsFor, go, goBack, goForward, goUp, moveTo, startNav, type Nav } from "./lib/nav";
import { useDirs } from "./lib/useDir";
import { Sidebar } from "./components/Chrome";
import { Toolbar, type View } from "./components/Toolbar";
import { useToast } from "./ui/ui";
import { ColumnView } from "./views/ColumnView";
import { ListView, sortEntries, type Sort } from "./views/ListView";
import { PreviewPane, QuickLook } from "./views/Preview";

/** A per-window convenience kept in localStorage (it may be unavailable). */
function useStored<T>(key: string, initial: T): [T, (v: T | ((prev: T) => T)) => void] {
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

export function App({ boot }: { boot: Boot }) {
  const toast = useToast();
  const [nav, setNav] = useState<Nav>(() => startNav(boot.start));
  const [view, setView] = useStored<View>("view", "list");
  const [showHidden, setShowHidden] = useStored("hidden", false);
  const [showPreview, setShowPreview] = useStored("preview", true);
  const [sort, setSort] = useState<Sort>({ key: "name", asc: true });
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<{ entries: Entry[]; truncated: boolean } | null>(null);
  const [searching, setSearching] = useState(false);
  const [quickLook, setQuickLook] = useState(false);
  const [space, setSpace] = useState<Space | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const searchMode = query.trim() !== "";
  const columns = useMemo(
    () => (view === "columns" ? columnsFor(nav.location, boot.places) : [nav.location]),
    [view, nav.location, boot.places],
  );
  const { dirs, refresh } = useDirs(columns, showHidden);

  // Entries the arrow keys move through, and the folder they belong to.
  const listEntries = useMemo(() => sortEntries(dirs[nav.location]?.listing?.entries ?? [], sort), [dirs, nav.location, sort]);
  const foundEntries = useMemo(() => sortEntries(results?.entries ?? [], sort), [results, sort]);
  const activeDir =
    view === "columns" && nav.selected === nav.location && columns.length > 1 ? parentOf(nav.location) ?? nav.location : nav.location;
  const activeEntries: Entry[] = searchMode
    ? foundEntries
    : view === "list"
      ? listEntries
      : dirs[activeDir]?.listing?.entries ?? [];

  const selectedEntry: Entry | null = useMemo(() => {
    const sel = nav.selected;
    if (!sel) return null;
    if (searchMode) return results?.entries.find((e) => e.path === sel) ?? null;
    const parent = parentOf(sel);
    return (parent && dirs[parent]?.listing?.entries.find((e) => e.path === sel)) || null;
  }, [nav.selected, searchMode, results, dirs]);

  // --- navigation actions ---
  const navigate = useCallback((path: string, selected: string | null = null) => {
    setQuery("");
    setNav((n) => go(n, path, selected));
  }, []);

  const select = useCallback(
    (entry: Entry, columnDir?: string) => {
      if (view === "columns" && !searchMode) {
        setNav((n) => (entry.is_dir ? moveTo(n, entry.path, entry.path) : moveTo(n, columnDir ?? n.location, entry.path)));
      } else {
        setNav((n) => ({ ...n, selected: entry.path }));
      }
    },
    [view, searchMode],
  );

  const open = useCallback(
    (entry: Entry) => {
      if (entry.is_dir) {
        navigate(entry.path, view === "columns" ? entry.path : null);
      } else {
        api.openPath(entry.path).catch((e) => toast("error", errorText(e)));
      }
    },
    [navigate, view, toast],
  );

  const changeView = (v: View) => {
    setView(v);
    // Column view keeps the selected folder open in the last column.
    if (v === "columns" && selectedEntry?.is_dir) setNav((n) => moveTo(n, selectedEntry.path, selectedEntry.path));
    if (v === "list") setNav((n) => (n.selected === n.location ? { ...n, selected: null } : n));
  };

  // --- search: debounced, results stream in batches; a newer search cancels
  // the running one in the backend and its late batches are ignored here ---
  useEffect(() => {
    if (!searchMode) {
      setResults(null);
      setSearching(false);
      return;
    }
    let alive = true;
    setSearching(true);
    const root = nav.location;
    const timer = window.setTimeout(() => {
      setResults({ entries: [], truncated: false });
      api
        .search(root, query, showHidden, (batch) => {
          if (alive) setResults((r) => ({ entries: [...(r?.entries ?? []), ...batch], truncated: r?.truncated ?? false }));
        })
        .then((summary) => {
          if (alive && !summary.cancelled) {
            setResults((r) => ({ entries: r?.entries ?? [], truncated: summary.truncated }));
            setSearching(false);
          }
        })
        .catch((e) => {
          if (alive) {
            setSearching(false);
            toast("error", errorText(e));
          }
        });
    }, 250);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [query, showHidden, nav.location, searchMode, toast]);

  // --- free space of the location's volume ---
  useEffect(() => {
    api.volume(nav.location).then(setSpace).catch(() => setSpace(null));
  }, [nav.location]);

  // Folders may change behind our back (the drive pulls): re-read on focus.
  useEffect(() => {
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [refresh]);

  // --- keyboard, Finder-style ---
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const typing = target?.tagName === "INPUT" || target?.tagName === "TEXTAREA";
      const mod = e.metaKey || e.ctrlKey;
      if (e.key === "Escape") {
        if (quickLook) setQuickLook(false);
        else if (query) setQuery("");
        searchRef.current?.blur();
        return;
      }
      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (typing && e.key !== "ArrowDown") return;
      if (typing && e.key === "ArrowDown") searchRef.current?.blur();

      if (mod && e.key === "[") return void (e.preventDefault(), setQuery(""), setNav(goBack));
      if (mod && e.key === "]") return void (e.preventDefault(), setQuery(""), setNav(goForward));
      if (mod && e.key === "ArrowUp") return void (e.preventDefault(), setQuery(""), setNav(goUp));
      if (mod && e.shiftKey && e.code === "Period") return void (e.preventDefault(), setShowHidden((h) => !h));
      if (mod && e.key === "2") return void (e.preventDefault(), changeView("list"));
      if (mod && e.key === "3") return void (e.preventDefault(), changeView("columns"));
      if (mod && e.key.toLowerCase() === "r") return void (e.preventDefault(), refresh());
      if ((mod && e.key === "ArrowDown") || e.key === "Enter") {
        e.preventDefault();
        if (selectedEntry) open(selectedEntry);
        return;
      }
      if (e.key === " ") {
        e.preventDefault();
        if (selectedEntry) setQuickLook((q) => !q);
        return;
      }
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        if (activeEntries.length === 0) return;
        const idx = activeEntries.findIndex((x) => x.path === nav.selected);
        const next =
          idx < 0 ? (e.key === "ArrowDown" ? 0 : activeEntries.length - 1) : Math.max(0, Math.min(activeEntries.length - 1, idx + (e.key === "ArrowDown" ? 1 : -1)));
        select(activeEntries[next], activeDir);
        return;
      }
      if (view === "columns" && !searchMode && (e.key === "ArrowRight" || e.key === "ArrowLeft")) {
        e.preventDefault();
        if (e.key === "ArrowRight" && nav.selected === nav.location) {
          const first = dirs[nav.location]?.listing?.entries[0];
          if (first) select(first, nav.location);
        } else if (e.key === "ArrowLeft") {
          if (nav.selected && nav.selected !== nav.location) setNav((n) => moveTo(n, n.location, n.location));
          else if (columns.length > 1) {
            const parent = parentOf(nav.location);
            if (parent) setNav((n) => moveTo(n, parent, parent));
          }
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const anchor = anchorOf(nav.location, boot.places);
  const crumbs = crumbsFor(nav.location, boot.places);
  const count = searchMode ? results?.entries.length ?? 0 : (dirs[nav.location]?.listing?.entries.length ?? 0);
  const sidePreview = (view === "list" || searchMode) && showPreview;

  return (
    <div className="surface flex">
      <div className="orb" style={{ width: 380, height: 380, left: 160, top: 140, background: "var(--a1)" }} />
      <div className="orb" style={{ width: 320, height: 320, right: 80, bottom: 40, background: "var(--a3)", opacity: 0.35 }} />
      <div className="orb" style={{ width: 260, height: 260, right: 380, top: 30, background: "var(--s1)", opacity: 0.22 }} />

      <Sidebar places={boot.places} active={anchor?.path ?? null} onPlace={(p) => navigate(p)} space={space} />

      <main className="relative z-10 flex min-w-0 flex-1 flex-col pb-4 pr-4">
        <Toolbar
          canBack={nav.back.length > 0}
          canForward={nav.forward.length > 0}
          canUp={parentOf(nav.location) != null}
          onBack={() => {
            setQuery("");
            setNav(goBack);
          }}
          onForward={() => {
            setQuery("");
            setNav(goForward);
          }}
          onUp={() => {
            setQuery("");
            setNav(goUp);
          }}
          crumbs={crumbs}
          onCrumb={(p) => navigate(p)}
          view={view}
          onView={changeView}
          showHidden={showHidden}
          onHidden={() => setShowHidden((h) => !h)}
          showPreview={showPreview}
          onPreview={() => setShowPreview((p) => !p)}
          query={query}
          onQuery={(q) => {
            setQuery(q);
            setNav((n) => ({ ...n, selected: null }));
          }}
          searching={searching}
          searchRef={searchRef}
        />

        <div className="flex min-h-0 flex-1 gap-4">
          <section className="glass flex min-w-0 flex-1 flex-col overflow-hidden">
            {searchMode ? (
              <ListView
                entries={foundEntries}
                sort={sort}
                onSort={setSort}
                selected={nav.selected}
                onSelect={(e) => select(e)}
                onOpen={open}
                whereRoot={nav.location}
                empty={searching ? "Searching" : `Nothing named like "${query.trim()}" in ${baseName(nav.location)}`}
              />
            ) : view === "list" ? (
              <ListView
                entries={listEntries}
                sort={sort}
                onSort={setSort}
                selected={nav.selected}
                onSelect={(e) => select(e)}
                onOpen={open}
                empty={dirs[nav.location]?.error ?? (dirs[nav.location]?.loading ? "" : "Empty folder")}
              />
            ) : (
              <ColumnView
                columns={columns}
                dirs={dirs}
                selected={nav.selected}
                preview={selectedEntry && !selectedEntry.is_dir ? selectedEntry : null}
                onSelect={select}
                onOpen={open}
                platform={boot.platform}
              />
            )}
            <footer className="flex h-8 flex-none items-center justify-between border-t border-line px-5 text-[0.68rem] text-dim">
              <span>
                {searchMode
                  ? searching
                    ? `Searching ${baseName(nav.location)}: ${count} found so far`
                    : `${count}${results?.truncated ? "+" : ""} result${count === 1 ? "" : "s"} in ${baseName(nav.location)}`
                  : `${count} item${count === 1 ? "" : "s"}`}
                {selectedEntry && selectedEntry.path !== nav.location && <span className="text-muted"> · {selectedEntry.name}</span>}
              </span>
              <span>{showHidden ? "Hidden files shown" : ""}</span>
            </footer>
          </section>
          {sidePreview && (
            <div className="w-[20rem] flex-none">
              <PreviewPane
                entry={selectedEntry}
                platform={boot.platform}
                folder={searchMode ? undefined : { name: baseName(nav.location), items: count }}
              />
            </div>
          )}
        </div>
      </main>

      {quickLook && selectedEntry && <QuickLook entry={selectedEntry} platform={boot.platform} onClose={() => setQuickLook(false)} />}
    </div>
  );
}
