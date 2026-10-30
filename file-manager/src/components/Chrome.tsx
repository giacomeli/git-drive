import { getCurrentWindow } from "@tauri-apps/api/window";
import { Minus, Square, X } from "lucide-react";
import type { ReactNode } from "react";
import { t } from "../i18n";

export const isMac = typeof navigator !== "undefined" && /Mac/.test(navigator.userAgent);

/** Window buttons on Linux (no native decorations); macOS keeps its traffic lights. */
export function WindowButtons() {
  if (isMac) return null;
  const win = getCurrentWindow();
  const btn = (label: string, onClick: () => void, icon: ReactNode, danger = false) => (
    <button
      aria-label={label}
      onClick={onClick}
      className={`grid h-7 w-7 place-items-center rounded-full bg-white/8 text-muted transition-colors hover:text-ink ${
        danger ? "hover:bg-coral/80" : "hover:bg-white/16"
      }`}
    >
      {icon}
    </button>
  );
  return (
    <div className="flex items-center gap-1.5 pl-2">
      {btn(t("window.minimize"), () => win.minimize(), <Minus className="h-3.5 w-3.5" />)}
      {btn(t("window.maximize"), () => win.toggleMaximize(), <Square className="h-3 w-3" />)}
      {btn(t("common.close"), () => win.close(), <X className="h-3.5 w-3.5" />, true)}
    </div>
  );
}
