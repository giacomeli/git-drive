import { ExternalLink, Folder, FolderSearch, LoaderCircle, X } from "lucide-react";
import { useEffect, useState } from "react";
import { api, errorText, fileUrl, type Entry, type PreviewKind, type TextPreview } from "../lib/api";
import { dateLabel, iconFor, kindLabel } from "../lib/kinds";
import { bytesText } from "../ui/format";

/** What can be shown of a file: the backend classifies (extension, then content). */
function usePreviewKind(path: string): PreviewKind | null {
  const [kind, setKind] = useState<PreviewKind | null>(null);
  useEffect(() => {
    let alive = true;
    setKind(null);
    api
      .previewKind(path)
      .then((k) => alive && setKind(k))
      .catch(() => alive && setKind("other"));
    return () => {
      alive = false;
    };
  }, [path]);
  return kind;
}

function TextBody({ path, large }: { path: string; large: boolean }) {
  const [text, setText] = useState<TextPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    setText(null);
    setError(null);
    api
      .readText(path)
      .then((t) => alive && setText(t))
      .catch((e) => alive && setError(errorText(e)));
    return () => {
      alive = false;
    };
  }, [path]);
  if (error) return <p className="p-4 text-xs text-coral">{error}</p>;
  if (!text) return <Loading />;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <pre
        className={`scroll selectable min-h-0 flex-1 whitespace-pre-wrap break-words rounded-xl bg-black/25 p-4 font-mono leading-relaxed text-ink/90 ${
          large ? "text-[0.8rem]" : "text-[0.68rem]"
        }`}
      >
        {text.text || " "}
      </pre>
      {text.truncated && (
        <p className="mt-2 text-[0.66rem] text-dim">
          Showing the first {bytesText(text.text.length)} of {bytesText(text.bytes)}; open the file for the rest.
        </p>
      )}
    </div>
  );
}

function Loading() {
  return (
    <div className="grid h-full place-items-center text-dim">
      <LoaderCircle className="spin h-5 w-5" />
    </div>
  );
}

function BigIcon({ entry, note }: { entry: Entry; note?: string }) {
  const { Icon, color } = iconFor(entry);
  return (
    <div className="grid h-full place-items-center text-center">
      <div>
        <Icon className="mx-auto h-20 w-20" style={{ color, filter: `drop-shadow(0 10px 24px ${color === "var(--a2)" ? "rgb(var(--a2-rgb) / 0.5)" : "rgb(0 0 0 / 0.35)"})` }} strokeWidth={1.3} />
        {note && <p className="mt-4 max-w-[16rem] text-[0.72rem] text-dim">{note}</p>}
      </div>
    </div>
  );
}

/** The visual part of a preview, by kind. */
export function PreviewBody({ entry, platform, large = false }: { entry: Entry; platform: "macOS" | "Linux"; large?: boolean }) {
  const kind = usePreviewKind(entry.path);
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [entry.path]);
  if (!kind) return <Loading />;
  const src = fileUrl(entry.path);
  if (failed) return <BigIcon entry={entry} note="This file cannot be shown here." />;
  switch (kind) {
    case "image":
      return (
        <div className="grid h-full place-items-center">
          <img
            key={entry.path}
            src={src}
            alt={entry.name}
            onError={() => setFailed(true)}
            className="max-h-full max-w-full rounded-xl object-contain shadow-2xl shadow-black/40"
          />
        </div>
      );
    case "video":
      return (
        <div className="grid h-full place-items-center">
          <video key={entry.path} src={src} controls onError={() => setFailed(true)} className="max-h-full max-w-full rounded-xl bg-black shadow-2xl shadow-black/40" />
        </div>
      );
    case "audio":
      return (
        <div className="flex h-full flex-col items-center justify-center gap-5">
          <BigIcon entry={entry} />
          <audio key={entry.path} src={src} controls onError={() => setFailed(true)} className="w-full max-w-sm" />
        </div>
      );
    case "pdf":
      return platform === "macOS" ? (
        <iframe key={entry.path} src={src} title={entry.name} className="h-full w-full rounded-xl bg-white" />
      ) : (
        <BigIcon entry={entry} note="PDF preview is not available on Linux yet. Open it in its app." />
      );
    case "text":
      return <TextBody path={entry.path} large={large} />;
    case "directory":
      return <BigIcon entry={entry} />;
    default:
      return <BigIcon entry={entry} note="No preview for this kind of file." />;
  }
}

function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-3 py-1.5 text-[0.72rem]">
      <span className="w-20 flex-none text-dim">{label}</span>
      <span className="selectable min-w-0 flex-1 break-words text-ink/90">{value}</span>
    </div>
  );
}

/** Preview with the file's details and actions (list view side pane, last column). */
export function PreviewPane({
  entry,
  platform,
  bare = false,
  folder,
}: {
  entry: Entry | null;
  platform: "macOS" | "Linux";
  bare?: boolean;
  /** Shown when nothing is selected: the folder in view. */
  folder?: { name: string; items: number };
}) {
  if (!entry) {
    return (
      <div className={`grid h-full place-items-center px-6 text-center ${bare ? "" : "glass"}`}>
        <div>
          <Folder className="mx-auto h-20 w-20" style={{ color: "var(--a2)", filter: "drop-shadow(0 10px 24px rgb(var(--a2-rgb) / 0.45))" }} strokeWidth={1.3} />
          {folder && (
            <>
              <p className="mt-4 text-[0.95rem] font-semibold">{folder.name}</p>
              <p className="mt-0.5 text-[0.72rem] text-muted">
                {folder.items} item{folder.items === 1 ? "" : "s"}
              </p>
            </>
          )}
          <p className="mt-3 text-[0.7rem] text-dim">Select a file to preview it</p>
        </div>
      </div>
    );
  }
  return (
    <div className={`flex h-full min-h-0 flex-col p-4 ${bare ? "" : "glass"}`}>
      <div className="h-64 flex-none">
        <PreviewBody entry={entry} platform={platform} />
      </div>
      <div className="mt-4 min-h-0 flex-1 overflow-y-auto">
        <p className="selectable break-words text-[0.95rem] font-semibold leading-snug">{entry.name}</p>
        <p className="mt-0.5 text-[0.72rem] text-muted">
          {kindLabel(entry)}
          {!entry.is_dir && ` · ${bytesText(entry.size)}`}
        </p>
        <div className="mt-3 border-t border-line pt-2">
          <Meta label="Modified" value={dateLabel(entry.modified)} />
          <Meta label="Where" value={entry.path.slice(0, entry.path.lastIndexOf("/")) || "/"} />
          {entry.kind === "symlink" && <Meta label="Type" value="Symbolic link" />}
        </div>
      </div>
      <Actions entry={entry} />
    </div>
  );
}

function Actions({ entry }: { entry: Entry }) {
  return (
    <div className="mt-3 flex flex-none gap-2">
      <button className="btn btn-primary flex-1" onClick={() => api.openPath(entry.path)}>
        <ExternalLink className="h-3.5 w-3.5" />
        Open
      </button>
      <button className="btn btn-ghost" onClick={() => api.openPath(entry.path, true)} title="Show in the system file manager">
        <FolderSearch className="h-3.5 w-3.5" />
        Reveal
      </button>
    </div>
  );
}

/** Space-bar preview over the whole window, as Finder's Quick Look. */
export function QuickLook({ entry, platform, onClose }: { entry: Entry; platform: "macOS" | "Linux"; onClose: () => void }) {
  return (
    <div className="fade-in fixed inset-0 z-40 grid place-items-center bg-black/35 p-10 backdrop-blur-md" onMouseDown={onClose}>
      <div className="glass glass-strong flex h-full max-h-[52rem] w-full max-w-[72rem] flex-col p-5" onMouseDown={(e) => e.stopPropagation()}>
        <header className="mb-4 flex flex-none items-center gap-3">
          <div className="min-w-0 flex-1">
            <p className="truncate text-[0.95rem] font-semibold">{entry.name}</p>
            <p className="text-[0.7rem] text-muted">
              {kindLabel(entry)}
              {!entry.is_dir && ` · ${bytesText(entry.size)}`} · {dateLabel(entry.modified)}
            </p>
          </div>
          <button className="btn btn-ghost" onClick={() => api.openPath(entry.path)}>
            <ExternalLink className="h-3.5 w-3.5" />
            Open
          </button>
          <button aria-label="Close" onClick={onClose} className="grid h-9 w-9 place-items-center rounded-full bg-white/8 text-muted hover:bg-white/14 hover:text-ink">
            <X className="h-4 w-4" />
          </button>
        </header>
        <div className="min-h-0 flex-1">
          <PreviewBody entry={entry} platform={platform} large />
        </div>
      </div>
    </div>
  );
}
