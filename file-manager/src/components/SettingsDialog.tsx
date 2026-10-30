import { Search, X } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { api, errorText } from "../lib/api";
import { ACCENTS } from "../ui/accent";
import { Segmented, Toggle, useToast } from "../ui/ui";
import type { Appearance } from "../lib/theme";
import { LANGUAGES, resolve, t, useI18n, type LanguageChoice } from "../i18n";
import { isMac } from "./Chrome";
import { TERMINAL_KEYS } from "../lib/shortcuts";
import { LIMIT_CHOICES, setPreviewLimit, usePreviewLimit, type PreviewPlace } from "../lib/previewLimits";
import { limitText } from "../views/Preview";

/** One setting: where it sits, its words (what the search looks in) and its control. */
interface Setting {
  id: string;
  section: string;
  title: string;
  description: string;
  /** More words the search finds it by. */
  keywords?: string;
  control: ReactNode;
}

/** Lowercase, without accents: "Côr" finds "cor". */
const fold = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();

/** The settings whose words hold every word of `query`. */
export function matching<T extends { section: string; title: string; description: string; keywords?: string }>(all: T[], query: string): T[] {
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return all;
  return all.filter((s) => {
    const text = fold(`${s.section} ${s.title} ${s.description} ${s.keywords ?? ""}`);
    return words.every((w) => text.includes(w));
  });
}

export interface WindowPrefs {
  photoMap: boolean;
  folderPreviews: boolean;
  folderPreviewsDrive: boolean;
  statusBar: boolean;
  preview: boolean;
  sidebar: boolean;
  terminal: boolean;
}

/**
 * Settings over the window, as Quick Look shows a file: the files stay
 * behind, dimmed. A search field filters the settings as one types (all its
 * words, accents ignored); each section shows only what matched. Esc clears
 * the search, then closes; so do Cmd+, and a click outside.
 */
export function SettingsDialog({
  accent,
  onAccent,
  appearance,
  onAppearance,
  systemLocales,
  prefs,
  onPref,
  onClose,
}: {
  accent: string;
  onAccent: (id: string) => void;
  appearance: Appearance;
  onAppearance: (a: Appearance) => void;
  /** The system's preferred languages: "System" names the one they lead to. */
  systemLocales: readonly string[];
  prefs: WindowPrefs;
  onPref: (key: keyof WindowPrefs, value: boolean) => void;
  onClose: () => void;
}) {
  const [query, setQuery] = useState("");
  const search = useRef<HTMLInputElement | null>(null);
  const latest = useRef({ query, onClose });
  latest.current = { query, onClose };

  useEffect(() => {
    search.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      const closeKey = (isMac ? e.metaKey : e.ctrlKey) && e.key === ",";
      if (e.key !== "Escape" && !closeKey) return;
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape" && latest.current.query) setQuery("");
      else latest.current.onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const i18n = useI18n();
  const systemName = LANGUAGES.find((l) => l.id === resolve("system", systemLocales))?.name ?? "English";
  const all: Setting[] = [
    {
      id: "language",
      section: t("settings.section.general"),
      title: t("settings.language.title"),
      description: t("settings.language.description"),
      keywords: t("settings.language.keywords"),
      control: (
        <select
          aria-label={t("settings.language.title")}
          className="field !h-8 w-auto !pr-9 !text-[0.76rem]"
          value={i18n.choice}
          onChange={(e) => i18n.setChoice(e.target.value as LanguageChoice)}
        >
          <option value="system">{t("settings.language.system", { name: systemName })}</option>
          {LANGUAGES.map((l) => (
            <option key={l.id} value={l.id}>
              {l.name}
            </option>
          ))}
        </select>
      ),
    },
    {
      id: "appearance",
      section: t("settings.section.appearance"),
      title: t("settings.theme.title"),
      description: t("settings.theme.description"),
      keywords: t("settings.theme.keywords"),
      control: (
        <Segmented<Appearance>
          options={["system", "light", "dark"] as const}
          value={appearance}
          onChange={onAppearance}
          labels={{ system: t("settings.theme.system"), light: t("settings.theme.light"), dark: t("settings.theme.dark") }}
        />
      ),
    },
    {
      id: "accent",
      section: t("settings.section.appearance"),
      title: t("settings.accent.title"),
      description: t("settings.accent.description"),
      keywords: t("settings.accent.keywords"),
      control: <AccentPicker accent={accent} onAccent={onAccent} />,
    },
    {
      id: "folderPreviews",
      section: t("settings.section.iconView"),
      title: t("settings.folderPreviews.title"),
      description: t("settings.folderPreviews.description"),
      keywords: t("settings.folderPreviews.keywords"),
      control: <Toggle checked={prefs.folderPreviews} onChange={(v) => onPref("folderPreviews", v)} />,
    },
    {
      id: "folderPreviewsDrive",
      section: t("settings.section.iconView"),
      title: t("settings.folderPreviewsDrive.title"),
      description: t("settings.folderPreviewsDrive.description"),
      keywords: t("settings.folderPreviewsDrive.keywords"),
      control: <Toggle checked={prefs.folderPreviews && prefs.folderPreviewsDrive} disabled={!prefs.folderPreviews} onChange={(v) => onPref("folderPreviewsDrive", v)} />,
    },
    {
      id: "previewLimitPanel",
      section: t("settings.section.preview"),
      title: t("settings.previewLimitPanel.title"),
      description: t("settings.previewLimitPanel.description"),
      keywords: t("settings.previewLimit.keywords"),
      control: <LimitSelect place="panel" label={t("settings.previewLimitPanel.title")} />,
    },
    {
      id: "previewLimitQuickLook",
      section: t("settings.section.preview"),
      title: t("settings.previewLimitQuickLook.title"),
      description: t("settings.previewLimitQuickLook.description"),
      keywords: t("settings.previewLimit.keywords"),
      control: <LimitSelect place="quickLook" label={t("settings.previewLimitQuickLook.title")} />,
    },
    {
      id: "photoMap",
      section: t("settings.section.photos"),
      title: t("settings.photoMap.title"),
      description: t("settings.photoMap.description"),
      keywords: t("settings.photoMap.keywords"),
      control: <Toggle checked={prefs.photoMap} onChange={(v) => onPref("photoMap", v)} />,
    },
    {
      id: "statusBar",
      section: t("settings.section.window"),
      title: t("settings.statusBar.title"),
      description: t("settings.statusBar.description"),
      keywords: t("settings.statusBar.keywords"),
      control: <Toggle checked={prefs.statusBar} onChange={(v) => onPref("statusBar", v)} />,
    },
    {
      id: "preview",
      section: t("settings.section.window"),
      title: t("settings.preview.title"),
      description: t("settings.preview.description", { keys: `${isMac ? "Cmd" : "Ctrl"}+Shift+P` }),
      keywords: t("settings.preview.keywords"),
      control: <Toggle checked={prefs.preview} onChange={(v) => onPref("preview", v)} />,
    },
    {
      id: "sidebar",
      section: t("settings.section.window"),
      title: t("settings.sidebar.title"),
      description: t("settings.sidebar.description"),
      keywords: t("settings.sidebar.keywords"),
      control: <Toggle checked={prefs.sidebar} onChange={(v) => onPref("sidebar", v)} />,
    },
    {
      id: "terminal",
      section: t("settings.section.window"),
      title: t("settings.terminal.title"),
      description: t("settings.terminal.description", { keys: TERMINAL_KEYS }),
      keywords: t("settings.terminal.keywords"),
      control: <Toggle checked={prefs.terminal} onChange={(v) => onPref("terminal", v)} />,
    },
  ];
  const found = matching(all, query);
  const sections = [...new Set(found.map((s) => s.section))];

  return createPortal(
    <div className="fade-in fixed inset-0 z-40 grid place-items-center bg-black/35 p-10 backdrop-blur-md" onMouseDown={onClose}>
      <div
        role="dialog"
        aria-modal
        aria-label={t("settings.title")}
        className="glass sheet flex h-[min(38rem,calc(100vh-5rem))] w-[min(46rem,100%)] flex-col p-5"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <header className="flex flex-none items-center gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="text-[1.05rem] font-semibold leading-tight">{t("settings.title")}</h2>
            <p className="mt-0.5 text-[0.72rem] text-muted">{t("settings.subtitle")}</p>
          </div>
          <label className="relative flex-none">
            <Search className="pointer-events-none absolute left-3.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dim" />
            <input
              ref={search}
              aria-label={t("settings.search")}
              className="field !h-9 w-60 !rounded-full !pl-9 !pr-8"
              placeholder={t("settings.search")}
              value={query}
              spellCheck={false}
              onChange={(e) => setQuery(e.target.value)}
            />
            {query && (
              <button aria-label={t("settings.clearSearch")} onClick={() => setQuery("")} className="absolute right-2.5 top-1/2 -translate-y-1/2 text-dim hover:text-ink">
                <X className="h-3.5 w-3.5" />
              </button>
            )}
          </label>
          <button aria-label={t("common.close")} onClick={onClose} className="grid h-9 w-9 flex-none place-items-center rounded-full bg-white/8 text-muted hover:bg-white/14 hover:text-ink">
            <X className="h-4 w-4" />
          </button>
        </header>
        <div className="scroll fade-edges -mx-1 mt-4 min-h-0 flex-1 px-1 py-3">
          {sections.length === 0 ? (
            <p className="grid h-full place-items-center text-[0.8rem] text-dim">{t("settings.noMatch", { query: query.trim() })}</p>
          ) : (
            sections.map((section) => (
              <section key={section} className="mb-5 last:mb-0">
                <p className="label mb-2 px-1">{section}</p>
                <div className="divide-y divide-white/6 rounded-2xl bg-white/[0.04] ring-1 ring-white/8">
                  {found
                    .filter((s) => s.section === section)
                    .map((s) => (
                      <div key={s.id} data-setting={s.id} className="flex items-center gap-6 px-4 py-3.5">
                        <div className="min-w-0 flex-1">
                          <p className="text-[0.84rem] font-medium">{s.title}</p>
                          <p className="mt-0.5 text-[0.7rem] text-dim">{s.description}</p>
                        </div>
                        <div className="flex-none">{s.control}</div>
                      </div>
                    ))}
                </div>
              </section>
            ))
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}

/** Accent picker in the style of the macOS one, as in the Control Center's
 * Settings: applies at once (the app applies `accent`) and is saved right away. */
/** The size limit of a preview (the panel or Quick Look), saved as it changes. */
function LimitSelect({ place, label }: { place: PreviewPlace; label: string }) {
  const limit = usePreviewLimit(place);
  return (
    <select aria-label={label} className="field !h-8 w-auto !pr-9 !text-[0.76rem]" value={limit} onChange={(e) => setPreviewLimit(place, Number(e.target.value))}>
      {LIMIT_CHOICES[place].map((bytes) => (
        <option key={bytes} value={bytes}>
          {limitText(bytes)}
        </option>
      ))}
    </select>
  );
}

function AccentPicker({ accent, onAccent }: { accent: string; onAccent: (id: string) => void }) {
  const toast = useToast();
  const choose = (id: string) => {
    if (id === accent) return;
    const previous = accent;
    onAccent(id);
    api.saveAccent(id).catch((e) => {
      onAccent(previous);
      toast("error", t("settings.accent.saveFailed", { error: errorText(e) }));
    });
  };
  return (
    <div className="flex flex-wrap justify-end gap-x-3 gap-y-1 pt-1" role="radiogroup" aria-label={t("settings.accent.title")}>
      {ACCENTS.map((a) => {
        const selected = a.id === accent;
        return (
          <div key={a.id} className="flex w-6 flex-col items-center">
            <button
              type="button"
              role="radio"
              aria-checked={selected}
              aria-label={a.label}
              title={a.label}
              onClick={() => choose(a.id)}
              className="h-6 w-6 rounded-full transition-transform duration-150 hover:scale-110 active:scale-95"
              style={{
                background: a.swatch,
                boxShadow: "inset 0 0 0 1px rgb(255 255 255 / 0.18), 0 4px 10px -4px rgb(0 0 0 / 0.5)",
                outline: selected ? `2.5px solid ${a.id === "multicolor" ? "var(--a2)" : a.swatch}` : "none",
                outlineOffset: 3,
              }}
            />
            <span className={`mt-2 whitespace-nowrap text-[0.62rem] font-medium text-muted transition-opacity ${selected ? "opacity-100" : "opacity-0"}`}>
              {a.label}
            </span>
          </div>
        );
      })}
    </div>
  );
}
