import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import { api, errorText, type About, type DaemonAction } from "./lib/api";
import { isMac, usePoll } from "./lib/hooks";
import { Sidebar, TitleBar, type Page } from "./components/Shell";
import { useToast } from "./components/ui";
import { AboutView } from "./views/AboutView";
import { LogsView } from "./views/LogsView";
import { OverviewView } from "./views/OverviewView";
import { SettingsView } from "./views/SettingsView";

const PAGES: Page[] = ["overview", "logs", "settings", "about"];

/** `--page <name>` on the command line opens the window at that page (URL hash). */
function initialPage(): Page {
  const hash = window.location.hash.replace("#", "") as Page;
  return PAGES.includes(hash) ? hash : "overview";
}

const DONE: Record<DaemonAction, string> = {
  start: "Daemon starting. The drive appears once its files are indexed.",
  stop: "Daemon stopped. Every change was committed before the drive unmounted.",
  restart: "Daemon restarting. The drive appears once its files are indexed.",
};

export function App() {
  const toast = useToast();
  const [page, setPage] = useState<Page>(initialPage);
  const [busy, setBusy] = useState<DaemonAction | null>(null);
  const [about, setAbout] = useState<About | null>(null);
  const { data: overview, refresh } = usePoll(api.overview, 2000);

  // A second launch (tray "Show log") navigates the open window.
  useEffect(() => {
    const off = listen<string>("navigate", (e) => {
      if (PAGES.includes(e.payload as Page)) setPage(e.payload as Page);
    });
    return () => {
      off.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    api.about().then(setAbout).catch(() => {});
  }, [overview?.service]);

  // Body classes drive the platform look (rounded window on Linux, tint over native glass on macOS).
  useEffect(() => {
    const cls = document.body.classList;
    cls.toggle("platform-linux", !isMac);
    cls.toggle("glass-native", about != null && about.glass !== "none");
  }, [about]);

  const onDaemon = useCallback(
    async (action: DaemonAction) => {
      setBusy(action);
      try {
        await api.daemon(action);
        toast("ok", DONE[action]);
      } catch (e) {
        toast("error", errorText(e));
      } finally {
        setBusy(null);
        refresh();
      }
    },
    [refresh, toast],
  );

  const running = overview?.service === "running";
  return (
    <div className="surface flex">
      <div className="orb" style={{ width: 380, height: 380, left: 160, top: 120, background: "var(--a1)" }} />
      <div className="orb" style={{ width: 320, height: 320, right: 80, bottom: 40, background: "var(--a3)", opacity: 0.35 }} />
      <div className="orb" style={{ width: 260, height: 260, right: 360, top: 40, background: "var(--s1)", opacity: 0.22 }} />
      <TitleBar />
      <Sidebar page={page} onPage={setPage} overview={overview} busy={busy} onDaemon={onDaemon} />
      <main className={`relative z-10 flex min-w-0 flex-1 flex-col pt-12`}>
        <div className={`min-h-0 flex-1 px-8 pb-6 ${page === "logs" ? "flex flex-col" : "scroll"}`}>
          {page === "overview" && <OverviewView overview={overview} />}
          {page === "logs" && <LogsView daemonRunning={running} />}
          {page === "settings" && <SettingsView daemonRunning={running} />}
          {page === "about" && <AboutView about={about} />}
        </div>
      </main>
    </div>
  );
}
