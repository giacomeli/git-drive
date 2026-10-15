import { getCurrentWindow } from "@tauri-apps/api/window";
import { Download, FileText, HardDrive, House, Laptop, Minus, Monitor, Square, X, type LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { Place, Space } from "../lib/api";
import { BrandLogo } from "../ui/Brand";
import { bytesText } from "../ui/format";

export const isMac = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);

const PLACE_ICON: Record<Place["icon"], LucideIcon> = {
  drive: HardDrive,
  home: House,
  desktop: Monitor,
  documents: FileText,
  downloads: Download,
  computer: Laptop,
};

/** Window buttons on Linux (no native decorations); macOS keeps its traffic lights. */
export function WindowButtons() {
  if (isMac) return null;
  const win = getCurrentWindow();
  const btn = (label: string, onClick: () => void, icon: ReactNode, danger = false) => (
    <button
      aria-label={label}
      onClick={onClick}
      className={`grid h-7 w-7 place-items-center rounded-full bg-white/8 text-muted transition-colors hover:text-white ${
        danger ? "hover:bg-coral/80" : "hover:bg-white/16"
      }`}
    >
      {icon}
    </button>
  );
  return (
    <div className="flex items-center gap-1.5 pl-2">
      {btn("Minimize", () => win.minimize(), <Minus className="h-3.5 w-3.5" />)}
      {btn("Maximize", () => win.toggleMaximize(), <Square className="h-3 w-3" />)}
      {btn("Close", () => win.close(), <X className="h-3.5 w-3.5" />, true)}
    </div>
  );
}

export function Sidebar({
  places,
  active,
  onPlace,
  space,
}: {
  places: Place[];
  active: string | null;
  onPlace: (path: string) => void;
  space: Space | null;
}) {
  const used = space ? 1 - space.free / Math.max(space.total, 1) : 0;
  return (
    <aside data-tauri-drag-region className={`relative z-10 flex w-56 flex-none flex-col px-3.5 pb-4 ${isMac ? "pt-14" : "pt-6"}`}>
      <div className="mb-7 px-2">
        <BrandLogo caption="Files" />
      </div>
      <p className="label mb-2 px-3">Places</p>
      <nav className="flex flex-col gap-0.5">
        {places.map((p) => {
          const Icon = PLACE_ICON[p.icon];
          const on = p.path === active;
          return (
            <button
              key={p.path}
              onClick={() => onPlace(p.path)}
              title={p.path}
              className={`relative flex h-9 items-center gap-3 rounded-xl px-3 text-[0.82rem] font-medium transition-all ${
                on ? "bg-white/10 text-white" : "text-muted hover:bg-white/5 hover:text-ink"
              }`}
            >
              {on && <span className="absolute left-0 top-1/2 h-4 w-1 -translate-y-1/2 rounded-full" style={{ background: "var(--accent)" }} />}
              <Icon className={`h-4 w-4 ${on ? "accent-fg" : ""}`} strokeWidth={2} />
              <span className="truncate">{p.label}</span>
            </button>
          );
        })}
      </nav>
      {space && (
        <div className="glass mt-auto p-3.5" style={{ borderRadius: 18 }}>
          <p className="text-[0.75rem] font-semibold">{bytesText(space.free)} free</p>
          <p className="mt-0.5 text-[0.66rem] text-dim">of {bytesText(space.total)} on this volume</p>
          <div className="mt-2.5 h-1.5 overflow-hidden rounded-full bg-white/8">
            <div className="h-full rounded-full" style={{ width: `${Math.max(used * 100, 4)}%`, background: "var(--accent)" }} />
          </div>
        </div>
      )}
    </aside>
  );
}
