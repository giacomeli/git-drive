import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  HardDrive,
  Info,
  LayoutDashboard,
  LoaderCircle,
  Minus,
  Play,
  RotateCw,
  ScrollText,
  SlidersHorizontal,
  Square,
  X,
} from "lucide-react";
import type { ReactNode } from "react";
import type { DaemonAction, Overview } from "../lib/api";
import { duration } from "../lib/format";
import { booting, health } from "../lib/health";
import { isMac } from "../lib/hooks";
import { StatusDot } from "./ui";

export type Page = "overview" | "logs" | "settings" | "about";

const NAV: { id: Page; label: string; icon: typeof LayoutDashboard }[] = [
  { id: "overview", label: "Overview", icon: LayoutDashboard },
  { id: "logs", label: "Logs", icon: ScrollText },
  { id: "settings", label: "Settings", icon: SlidersHorizontal },
  { id: "about", label: "About", icon: Info },
];

/** Drag strip along the top; on Linux (no native decorations) it carries the window buttons. */
export function TitleBar() {
  const win = getCurrentWindow();
  return (
    <div data-tauri-drag-region className="absolute inset-x-0 top-0 z-30 flex h-11 items-center justify-end px-3">
      {!isMac && (
        <div className="flex items-center gap-1.5">
          <WindowButton label="Minimize" onClick={() => win.minimize()}>
            <Minus className="h-3.5 w-3.5" />
          </WindowButton>
          <WindowButton label="Maximize" onClick={() => win.toggleMaximize()}>
            <Square className="h-3 w-3" />
          </WindowButton>
          <WindowButton label="Close" onClick={() => win.close()} danger>
            <X className="h-3.5 w-3.5" />
          </WindowButton>
        </div>
      )}
    </div>
  );
}

function WindowButton({
  label,
  onClick,
  children,
  danger = false,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
  danger?: boolean;
}) {
  return (
    <button
      aria-label={label}
      onClick={onClick}
      className={`grid h-7 w-7 place-items-center rounded-full bg-white/8 text-muted transition-colors hover:text-white ${
        danger ? "hover:bg-coral/80" : "hover:bg-white/16"
      }`}
    >
      {children}
    </button>
  );
}

export function Sidebar({
  page,
  onPage,
  overview,
  busy,
  onDaemon,
}: {
  page: Page;
  onPage: (page: Page) => void;
  overview: Overview | null;
  busy: DaemonAction | null;
  onDaemon: (action: DaemonAction) => void;
}) {
  const h = health(overview);
  const starting = booting(overview);
  const s = overview?.status;
  const running = overview?.service === "running";
  const installed = overview != null && overview.service !== "not_installed";
  const device = s?.p2p?.device_name;
  return (
    <aside className={`relative z-10 flex w-60 flex-none flex-col px-4 pb-4 ${isMac ? "pt-14" : "pt-6"}`}>
      <div className="mb-8 flex items-center gap-3 px-2">
        <div
          className="grid h-10 w-10 place-items-center rounded-2xl"
          style={{ background: "var(--accent)", boxShadow: "0 10px 24px -8px rgb(var(--a3-rgb) / 0.7)" }}
        >
          <HardDrive className="h-5 w-5 text-white" strokeWidth={2.2} />
        </div>
        <div className="min-w-0">
          <p className="text-[0.95rem] font-semibold leading-tight tracking-tight">git-drive</p>
          <p className="truncate text-[0.7rem] text-dim">{device ? `on ${device}` : "Control Center"}</p>
        </div>
      </div>

      <nav className="flex flex-col gap-1">
        {NAV.map(({ id, label, icon: Icon }) => {
          const active = id === page;
          return (
            <button
              key={id}
              onClick={() => onPage(id)}
              className={`group relative flex h-11 items-center gap-3 rounded-2xl px-3.5 text-[0.84rem] font-medium transition-all ${
                active ? "bg-white/10 text-white shadow-lg shadow-black/10" : "text-muted hover:bg-white/5 hover:text-ink"
              }`}
            >
              {active && (
                <span
                  className="absolute left-0 top-1/2 h-5 w-1 -translate-y-1/2 rounded-full"
                  style={{ background: "var(--accent)" }}
                />
              )}
              <Icon className={`h-[1.1rem] w-[1.1rem] ${active ? "accent-fg" : ""}`} strokeWidth={2} />
              {label}
            </button>
          );
        })}
      </nav>

      <div className="mt-auto">
        <div className="glass p-4" style={{ borderRadius: 20 }}>
          <div className="flex items-center gap-2.5">
            {starting ? <LoaderCircle className="spin h-3.5 w-3.5 text-cyan" /> : <StatusDot tone={h.tone} pulse={running} />}
            <p className="text-[0.82rem] font-semibold">
              {!installed ? "Not installed" : starting ? "Daemon starting" : running ? "Daemon running" : "Daemon stopped"}
            </p>
          </div>
          <p className="mt-1 truncate pl-6 text-[0.7rem] text-dim">
            {starting
              ? "Mounting the drive"
              : running && s && overview
                ? `pid ${s.pid} · up ${duration(overview.now - s.started_at)}`
                : installed
                  ? "Drive unmounted"
                  : "Run git-drive install"}
          </p>
          {installed && (
            <div className="mt-3.5 flex gap-2">
              {running ? (
                <>
                  <button className="btn btn-danger flex-1" disabled={busy != null} onClick={() => onDaemon("stop")}>
                    {busy === "stop" ? <LoaderCircle className="spin h-3.5 w-3.5" /> : <Square className="h-3 w-3" fill="currentColor" />}
                    {busy === "stop" ? "Stopping" : "Stop"}
                  </button>
                  <button
                    className="btn btn-ghost w-9 px-0"
                    aria-label="Restart"
                    title="Restart"
                    disabled={busy != null}
                    onClick={() => onDaemon("restart")}
                  >
                    <RotateCw className={`h-3.5 w-3.5 ${busy === "restart" ? "spin" : ""}`} />
                  </button>
                </>
              ) : (
                <button className="btn btn-primary flex-1" disabled={busy != null} onClick={() => onDaemon("start")}>
                  {busy === "start" ? <LoaderCircle className="spin h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" fill="currentColor" />}
                  {busy === "start" ? "Starting" : "Start daemon"}
                </button>
              )}
            </div>
          )}
        </div>
      </div>
    </aside>
  );
}

export function PageHeader({ title, subtitle, actions }: { title: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <header className="mb-6 flex items-end justify-between gap-4">
      <div className="min-w-0">
        <h1 className="text-[1.65rem] font-semibold leading-tight tracking-tight">{title}</h1>
        {subtitle && <p className="mt-1 truncate text-[0.8rem] text-muted">{subtitle}</p>}
      </div>
      {actions && <div className="flex flex-none items-center gap-2">{actions}</div>}
    </header>
  );
}
