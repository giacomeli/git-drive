import { BookOpen, Bug, ExternalLink, FolderGit2, Package, Scale, X, type LucideIcon } from "lucide-react";
import { useEffect } from "react";
import { createPortal } from "react-dom";
import { api, errorText, type Boot } from "../lib/api";
import { useToast } from "../ui/ui";
import { t, type Key } from "../i18n";

/** Where git-drive lives. */
const REPO = "https://github.com/giacomeli/git-drive";

/** Labels and details are message keys (read at render, in the chosen language); `detail` may be literal. */
const LINKS: { label: Key; detail: Key | { text: string }; url: string; Icon: LucideIcon }[] = [
  { label: "about.source", detail: { text: "github.com/giacomeli/git-drive" }, url: REPO, Icon: BookOpen },
  { label: "about.releases", detail: "about.releasesDetail", url: `${REPO}/releases`, Icon: Package },
  { label: "about.report", detail: "about.reportDetail", url: `${REPO}/issues`, Icon: Bug },
  { label: "about.license", detail: { text: "MIT" }, url: `${REPO}/blob/main/LICENSE`, Icon: Scale },
];

const GLASS: Record<Boot["glass"], Key> = { "liquid-glass": "about.glass.liquid", vibrancy: "about.glass.vibrancy", none: "about.glass.none" };

/** About git-drive Files: what it is, its version and where it was built for, and its links. */
export function AboutDialog({ boot, onClose }: { boot: Boot; onClose: () => void }) {
  const toast = useToast();
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  const facts: [string, string][] = [
    [t("about.version"), boot.version],
    [t("about.platform"), `${boot.platform} · ${boot.arch}`],
    [t("about.windowEffect"), t(GLASS[boot.glass])],
    [t("about.builtWith"), `Tauri ${boot.tauri}, React, xterm.js`],
  ];
  return createPortal(
    <div className="fade-in fixed inset-0 z-40 grid place-items-center bg-black/35 p-10 backdrop-blur-md" onMouseDown={onClose}>
      <div role="dialog" aria-modal aria-label={t("about.title")} className="glass sheet relative w-[min(30rem,100%)] p-6" onMouseDown={(e) => e.stopPropagation()}>
        <button aria-label={t("common.close")} onClick={onClose} className="absolute right-4 top-4 grid h-8 w-8 place-items-center rounded-full bg-white/8 text-muted hover:bg-white/14 hover:text-ink">
          <X className="h-4 w-4" />
        </button>
        <div className="flex items-center gap-4">
          <span className="grid h-16 w-16 flex-none place-items-center rounded-[1.3rem] shadow-lg shadow-black/20" style={{ background: "linear-gradient(135deg, var(--a1), var(--a3))" }}>
            <FolderGit2 className="h-8 w-8 text-paper" strokeWidth={1.8} />
          </span>
          <div className="min-w-0">
            <p className="text-[1.35rem] font-semibold leading-tight tracking-tight">git-drive Files</p>
            <p className="mt-1 text-[0.76rem] text-muted">{t("about.tagline")}</p>
          </div>
        </div>
        <dl className="mt-6 grid grid-cols-[7rem_minmax(0,1fr)] gap-x-3 gap-y-2 text-[0.78rem]">
          {facts.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-dim">{k}</dt>
              <dd className="selectable text-ink/90">{v}</dd>
            </div>
          ))}
        </dl>
        <div className="mt-6 divide-y divide-white/6 rounded-2xl bg-white/[0.04] ring-1 ring-white/8">
          {LINKS.map(({ label, detail, url, Icon }) => (
            <button
              key={label}
              onClick={() => api.openUrl(url).catch((e) => toast("error", errorText(e)))}
              className="flex w-full items-center gap-3 px-4 py-3 text-left transition-colors first:rounded-t-2xl last:rounded-b-2xl hover:bg-white/5"
            >
              <Icon className="h-4 w-4 flex-none accent-fg" />
              <span className="min-w-0 flex-1">
                <span className="block text-[0.8rem] font-medium">{t(label)}</span>
                <span className="block truncate text-[0.68rem] text-dim">{typeof detail === "string" ? t(detail) : detail.text}</span>
              </span>
              <ExternalLink className="h-3.5 w-3.5 flex-none text-dim" />
            </button>
          ))}
        </div>
      </div>
    </div>,
    document.body,
  );
}
