import { ExternalLink, ShieldAlert, ShieldCheck, Trash2 } from "lucide-react";
import { useState } from "react";
import { t } from "../i18n";

/** The Trash's bar above its items: what it holds and Empty Trash. */
export function TrashBar({ count, onEmpty }: { count: number; onEmpty: () => void }) {
  return (
    <div className="flex h-11 flex-none items-center justify-between gap-3 border-b border-line pl-5 pr-3">
      <span className="flex items-center gap-2 text-[0.78rem] font-semibold text-ink">
        <Trash2 className="h-4 w-4 text-muted" strokeWidth={1.9} />
        {t("views.trash")}
        <span className="font-normal text-dim">{t("common.item", { count })}</span>
      </span>
      <button type="button" className="btn btn-ghost !h-8 !px-3 !text-[0.74rem]" disabled={count === 0} onClick={onEmpty}>
        {t("views.emptyTrash")}
      </button>
    </div>
  );
}

/**
 * macOS keeps the Trash private (TCC): without Full Disk Access no app but
 * Finder can list it. No app can grant that to itself or ask for it with a
 * prompt, so the main button opens the list in System Settings; once the
 * user turns the app on, macOS offers to reopen it, and the Trash shows.
 * Finder stays one click away.
 */
export function TrashLocked({
  onAllow,
  onOpenInFinder,
  onShowApp,
}: {
  onAllow: () => void;
  onOpenInFinder: () => void;
  /** Reveals this app in Finder, to add it with + (absent outside a bundle). */
  onShowApp?: () => void;
}) {
  const [asked, setAsked] = useState(false);
  return (
    <div className="grid flex-1 place-items-center px-8">
      <div className="flex max-w-md flex-col items-center gap-3 text-center">
        <span className="grid h-12 w-12 place-items-center rounded-2xl bg-white/6 ring-1 ring-white/10">
          <ShieldAlert className="h-6 w-6 text-amber" strokeWidth={1.8} />
        </span>
        <h3 className="text-[0.92rem] font-semibold text-ink">{t("views.trashPrivate")}</h3>
        <p className="text-[0.76rem] leading-relaxed text-muted">{t("views.trashPrivateText")}</p>
        <div className="mt-1 flex flex-wrap justify-center gap-2">
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => {
              setAsked(true);
              onAllow();
            }}
          >
            <ShieldCheck className="h-3.5 w-3.5" />
            {t("views.allowFda")}
          </button>
          <button type="button" className="btn btn-ghost" onClick={onOpenInFinder}>
            <ExternalLink className="h-3.5 w-3.5" />
            {t("views.openInFinder")}
          </button>
        </div>
        {asked && (
          <div className="glass mt-2 w-full px-4 py-3 text-left" style={{ borderRadius: 14 }}>
            <ol className="list-decimal space-y-1 pl-4 text-[0.72rem] leading-relaxed text-muted">
              <li>{t("views.fdaStep1")}</li>
              <li>
                {t("views.fdaStep2")}
                {onShowApp && (
                  <>
                    {" "}
                    (
                    <button type="button" className="accent-fg underline-offset-2 hover:underline" onClick={onShowApp}>
                      {t("views.fdaShowApp")}
                    </button>{" "}
                    {t("views.fdaShowAppAfter")})
                  </>
                )}
                .
              </li>
              <li>{t("views.fdaStep3")}</li>
            </ol>
          </div>
        )}
      </div>
    </div>
  );
}
