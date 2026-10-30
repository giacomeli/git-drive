import { LoaderCircle } from "lucide-react";
import type { ComputerData, Place, RepoUsage, Volume } from "../lib/api";
import { limitNote, repoColor, repoText } from "../lib/repoUsage";
import { PLACE_ICON, VOLUME_ICON } from "../components/Sidebar";
import { bytesText } from "../ui/format";
import { t } from "../i18n";

/** Bar color by how full a volume is: the accent, amber above 80%, coral above 90%. */
function fillColor(used: number): string {
  if (used > 0.9) return "var(--color-coral)";
  if (used > 0.8) return "var(--color-amber)";
  return "var(--accent)";
}

function DirTile({ place, onOpen, onContext }: { place: Place; onOpen: () => void; onContext: (ev: React.MouseEvent) => void }) {
  const Icon = PLACE_ICON[place.icon];
  return (
    <button
      onClick={onOpen}
      onContextMenu={(ev) => {
        ev.preventDefault();
        onContext(ev);
      }}
      title={place.path}
      className="flex flex-col items-center gap-2.5 rounded-2xl border border-white/8 bg-white/[0.04] px-3 pb-3.5 pt-5 transition-colors hover:bg-white/10"
    >
      <span className="grid h-14 w-14 place-items-center rounded-2xl bg-white/6 ring-1 ring-white/10">
        <Icon className="h-7 w-7 accent-fg" strokeWidth={1.7} />
      </span>
      <span className="max-w-full truncate text-[0.8rem] font-medium text-ink">{place.label}</span>
    </button>
  );
}

/**
 * The drive's card: its repository against what the host accepts (the
 * daemon's measurement of the object store), and below it the disk the
 * repository lives on, which bounds the drive too.
 */
function DriveCard({ volume, repo, onOpen }: { volume: Volume; repo: RepoUsage; onOpen: () => void }) {
  const Icon = VOLUME_ICON[volume.kind];
  const share = repo.limit ? Math.min(repo.used / repo.limit, 1) : 0;
  return (
    <button
      onClick={onOpen}
      title={volume.path}
      className="flex items-center gap-4 rounded-2xl border border-white/8 bg-white/[0.04] px-4 py-3.5 text-left transition-colors hover:bg-white/10"
    >
      <span className="grid h-12 w-12 flex-none place-items-center rounded-xl bg-white/6 ring-1 ring-white/10">
        <Icon className="h-6 w-6 accent-fg" strokeWidth={1.7} />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-1.5">
        <span className="truncate text-[0.84rem] font-semibold text-ink">{volume.name}</span>
        <span className="flex justify-between gap-2 text-[0.7rem] text-muted">
          <span className="truncate tabular-nums">{repoText(repo)}</span>
          {repo.limit != null && <span className="flex-none tabular-nums text-dim">{Math.round((repo.used / repo.limit) * 100)}%</span>}
        </span>
        {repo.limit != null && (
          <span className="relative h-1.5 rounded-full bg-white/8" title={limitNote(repo)}>
            <span className="block h-full rounded-full" style={{ width: `${Math.max(share * 100, 3)}%`, background: repoColor(repo) }} />
            {repo.recommended != null && (
              <span
                className="absolute -top-0.5 h-2.5 w-0.5 rounded-full bg-white/70"
                style={{ left: `calc(${(repo.recommended / repo.limit) * 100}% - 1px)` }}
              />
            )}
          </span>
        )}
        {volume.total > 0 && (
          <span className="text-[0.66rem] text-dim tabular-nums">
            {t("views.diskFree", { free: bytesText(volume.free), total: bytesText(volume.total) })}
          </span>
        )}
      </span>
    </button>
  );
}

function DiskCard({ volume, onOpen }: { volume: Volume; onOpen: () => void }) {
  if (volume.repo) return <DriveCard volume={volume} repo={volume.repo} onOpen={onOpen} />;
  const Icon = VOLUME_ICON[volume.kind];
  const known = volume.total > 0;
  const used = known ? (volume.total - volume.free) / volume.total : 0;
  return (
    <button
      onClick={onOpen}
      title={volume.path}
      className="flex items-center gap-4 rounded-2xl border border-white/8 bg-white/[0.04] px-4 py-3.5 text-left transition-colors hover:bg-white/10"
    >
      <span className="grid h-12 w-12 flex-none place-items-center rounded-xl bg-white/6 ring-1 ring-white/10">
        <Icon className="h-6 w-6 accent-fg" strokeWidth={1.7} />
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-1.5">
        <span className="truncate text-[0.84rem] font-semibold text-ink">{volume.name}</span>
        {known ? (
          <>
            <span className="flex justify-between gap-2 text-[0.7rem] text-muted">
              <span className="tabular-nums">
                {bytesText(volume.total - volume.free)} / {bytesText(volume.total)}
              </span>
              <span className="tabular-nums text-dim">{t("views.free", { free: bytesText(volume.free) })}</span>
            </span>
            <span className="h-1.5 overflow-hidden rounded-full bg-white/8">
              <span className="block h-full rounded-full" style={{ width: `${Math.max(used * 100, 3)}%`, background: fillColor(used) }} />
            </span>
          </>
        ) : (
          <span className="text-[0.7rem] text-dim">{t("views.sizeUnknown")}</span>
        )}
      </span>
    </button>
  );
}

/**
 * The Computer view, as the Deepin file manager draws it: the home's
 * standard folders as large tiles, then every mounted volume with how full
 * it is. A click opens.
 */
export function ComputerView({
  data,
  onOpen,
  onDirMenu,
}: {
  data: ComputerData | null;
  onOpen: (path: string) => void;
  onDirMenu: (ev: React.MouseEvent, place: Place) => void;
}) {
  if (!data) {
    return (
      <div className="grid flex-1 place-items-center text-dim">
        <LoaderCircle className="spin h-5 w-5" />
      </div>
    );
  }
  return (
    <div className="scroll fade-edges min-h-0 flex-1 px-6 pb-6 pt-5">
      {data.dirs.length > 0 && (
        <>
          <h3 className="label mb-3">{t("views.myDirectories")}</h3>
          <div className="grid gap-3" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(8.5rem, 1fr))" }}>
            {data.dirs.map((d) => (
              <DirTile key={d.path} place={d} onOpen={() => onOpen(d.path)} onContext={(ev) => onDirMenu(ev, d)} />
            ))}
          </div>
        </>
      )}
      <h3 className={`label mb-3 ${data.dirs.length > 0 ? "mt-7" : ""}`}>{t("views.disks")}</h3>
      <div className="grid gap-3" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(17rem, 1fr))" }}>
        {data.volumes.map((v) => (
          <DiskCard key={v.path} volume={v} onOpen={() => onOpen(v.path)} />
        ))}
      </div>
    </div>
  );
}
