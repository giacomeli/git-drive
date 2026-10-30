import { Box, Eye, ExternalLink, LoaderCircle, X } from "lucide-react";
import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { api, errorText, fileUrl, type Entry, type PreviewKind, type TextPreview } from "../lib/api";
import { EntryGlyph } from "../components/Glyph";
import { formatJson } from "../lib/jsonView";
import { dateLabel, iconFor, kindLabel } from "../lib/kinds";
import { howToShow, usePreviewLimit } from "../lib/previewLimits";
import { useFolderPreview } from "../lib/useFolderPreview";
import { bytesText } from "../ui/format";
import { num, t } from "../i18n";
import { JsonLines } from "./JsonLines";
import { VideoPlayer } from "./VideoPlayer";

// Loaded on first use: marked and DOMPurify, and three.js, stay out of the first bundle.
const MarkdownView = lazy(() => import("./MarkdownView"));
const ModelViewer = lazy(() => import("./ModelViewer"));

/** A size limit as Settings lists it: "250 KB", "1 MB", "100 MB". */
export function limitText(bytes: number): string {
  return bytes >= 1_000_000 ? `${num(bytes / 1_000_000, 0)} MB` : `${num(bytes / 1_000, 0)} KB`;
}

/** Why a file past the preview's size limit shows as text or with its details. */
const overLimit = (limit: number) => t("views.overLimit", { limit: limitText(limit) });

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

/** A text file read for the preview: `max` bytes at most (the backend's default when absent). */
function useText(path: string, max?: number): { text: TextPreview | null; error: string | null } {
  const [state, setState] = useState<{ path: string; text: TextPreview | null; error: string | null }>({ path, text: null, error: null });
  useEffect(() => {
    let alive = true;
    setState({ path, text: null, error: null });
    api
      .readText(path, max)
      .then((text) => alive && setState({ path, text, error: null }))
      .catch((e) => alive && setState({ path, text: null, error: errorText(e) }));
    return () => {
      alive = false;
    };
  }, [path, max]);
  return state.path === path ? state : { text: null, error: null };
}

function TextBody({ path, large, note }: { path: string; large: boolean; note?: string }) {
  const { text, error } = useText(path);
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
      {note && <p className="mt-2 text-[0.66rem] text-dim">{note}</p>}
      {text.binary ? (
        <p className="mt-2 text-[0.66rem] text-dim">
          {t("views.textBinary", { total: bytesText(text.bytes) })}
        </p>
      ) : (
        text.truncated && (
          <p className="mt-2 text-[0.66rem] text-dim">
            {t("views.textTruncated", { shown: bytesText(text.text.length), total: bytesText(text.bytes) })}
          </p>
        )
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

/** Two small buttons over a preview's top right corner: rendered or as written. */
function ModeSwitch({ mode, onMode }: { mode: "rendered" | "source"; onMode: (m: "rendered" | "source") => void }) {
  return (
    <div className="absolute right-3 top-2 z-10 flex rounded-full bg-white/8 p-0.5 text-[0.62rem] ring-1 ring-white/10 backdrop-blur-md" role="radiogroup" aria-label={t("views.markdown.mode")}>
      {(["rendered", "source"] as const).map((m) => (
        <button
          key={m}
          type="button"
          role="radio"
          aria-checked={mode === m}
          tabIndex={-1}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => onMode(m)}
          className={`rounded-full px-2.5 py-0.5 font-medium transition-colors ${mode === m ? "bg-white/15 text-ink" : "text-dim hover:text-ink"}`}
        >
          {m === "rendered" ? t("views.markdown.rendered") : t("views.markdown.source")}
        </button>
      ))}
    </div>
  );
}

/** Markdown within the limit, rendered (or as written, by the switch); past it, as text. */
function MarkdownBody({ entry, large, limit }: { entry: Entry; large: boolean; limit: number }) {
  const render = howToShow("markdown", entry.size, limit) === "render";
  const { text, error } = useText(entry.path, render ? limit : undefined);
  const [mode, setMode] = useState<"rendered" | "source">("rendered");
  if (!render) return <TextBody path={entry.path} large={large} note={overLimit(limit)} />;
  if (error) return <p className="p-4 text-xs text-coral">{error}</p>;
  if (!text) return <Loading />;
  return (
    <div className="relative h-full min-h-0">
      <ModeSwitch mode={mode} onMode={setMode} />
      {mode === "rendered" ? (
        <Suspense fallback={<Loading />}>
          <MarkdownView text={text.text} path={entry.path} large={large} />
        </Suspense>
      ) : (
        <pre
          className={`scroll selectable h-full min-h-0 overflow-auto whitespace-pre-wrap break-words rounded-xl bg-black/25 p-4 pt-9 font-mono leading-relaxed text-ink/90 ${
            large ? "text-[0.8rem]" : "text-[0.68rem]"
          }`}
        >
          {text.text}
        </pre>
      )}
    </div>
  );
}

/** JSON within the limit that parses, formatted; anything else as text. */
function JsonBody({ entry, large, limit }: { entry: Entry; large: boolean; limit: number }) {
  const render = howToShow("json", entry.size, limit) === "render";
  const { text, error } = useText(entry.path, render ? limit : undefined);
  const lines = useMemo(() => (text && !text.truncated && !text.binary ? formatJson(text.text) : null), [text]);
  if (!render) return <TextBody path={entry.path} large={large} note={overLimit(limit)} />;
  if (error) return <p className="p-4 text-xs text-coral">{error}</p>;
  if (!text) return <Loading />;
  if (!lines) return <TextBody path={entry.path} large={large} />;
  return <JsonLines lines={lines} large={large} />;
}

/** A 3D model within the limit, drawn; past it, its details and a button to draw it anyway. */
function ModelBody({ entry, large, limit }: { entry: Entry; large: boolean; limit: number }) {
  const [anyway, setAnyway] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    setAnyway(false);
    setFailed(null);
  }, [entry.path]);
  if (failed) return <BigIcon entry={entry} note={failed} />;
  if (!anyway && howToShow("model", entry.size, limit) === "details") {
    return (
      <div className="grid h-full place-items-center text-center">
        <div>
          <BigIcon entry={entry} note={t("views.model.overLimit", { limit: limitText(limit) })} />
          <button className="btn btn-ghost mx-auto mt-3" onClick={() => setAnyway(true)}>
            <Box className="h-3.5 w-3.5" />
            {t("views.model.renderAnyway")}
          </button>
        </div>
      </div>
    );
  }
  return (
    <Suspense fallback={<Loading />}>
      <ModelViewer key={entry.path} path={entry.path} ext={entry.ext} large={large} onFail={setFailed} />
    </Suspense>
  );
}

/** A folder as the icon view draws it: the accent folder, with the previews
 * of what it holds over it (the same picks, at this size). */
function FolderArt({ entry, size }: { entry: Entry; size: number }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const { prints, fresh } = useFolderPreview(entry, size, ref);
  return (
    <div ref={ref} className="mx-auto" style={{ width: size, height: size, filter: "drop-shadow(0 10px 24px rgb(var(--a2-rgb) / 0.35))" }}>
      <EntryGlyph entry={entry} size={size} prints={prints} fresh={fresh} />
    </div>
  );
}

/** The folder in view, as an entry for its art (its date is unknown here: operations and the
 * window's return to the front still refresh its previews). */
const folderEntry = (path: string, name: string): Entry => ({
  name,
  path,
  kind: "dir",
  is_dir: true,
  size: 0,
  modified: null,
  created: null,
  hidden: false,
  ext: "",
  thumb: false,
});

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
  // Quick Look is opened on purpose: it processes larger files than the panel.
  const limit = usePreviewLimit(large ? "quickLook" : "panel");
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [entry.path]);
  if (!kind) return <Loading />;
  const src = fileUrl(entry.path);
  if (failed) return <BigIcon entry={entry} note={t("views.cannotShow")} />;
  switch (kind) {
    case "image":
      // Flex, not grid: in a grid's auto row a percentage max-height has
      // nothing definite to resolve against, and a portrait image grew past
      // its box over the name below it.
      return (
        <div className="flex h-full min-h-0 items-center justify-center">
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
      return <VideoPlayer key={entry.path} src={src} compact={!large} onError={() => setFailed(true)} />;
    case "audio":
      return (
        <div className="flex h-full flex-col items-center justify-center gap-5">
          <BigIcon entry={entry} />
          <audio key={entry.path} src={src} controls onError={() => setFailed(true)} className="w-full max-w-sm" />
        </div>
      );
    case "pdf":
      return platform === "macOS" ? (
        <iframe key={entry.path} src={src} title={entry.name} className="h-full w-full rounded-xl bg-paper" />
      ) : (
        <BigIcon entry={entry} note={t("views.pdfLinux")} />
      );
    case "text":
      return <TextBody path={entry.path} large={large} />;
    case "markdown":
      return <MarkdownBody key={entry.path} entry={entry} large={large} limit={limit} />;
    case "json":
      return <JsonBody key={entry.path} entry={entry} large={large} limit={limit} />;
    case "model":
      return <ModelBody entry={entry} large={large} limit={limit} />;
    case "directory":
      return (
        <div className="grid h-full place-items-center">
          <FolderArt entry={entry} size={large ? 288 : 160} />
        </div>
      );
    default:
      return <BigIcon entry={entry} note={t("views.noPreview")} />;
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

/** Several items selected: their icons in a pile, how many there are, and the size of the files. */
export function MultiPreview({ entries, bare = false }: { entries: Entry[]; bare?: boolean }) {
  const folders = entries.filter((e) => e.is_dir).length;
  const files = entries.length - folders;
  const size = entries.reduce((sum, e) => sum + (e.is_dir ? 0 : e.size), 0);
  const counts = [folders > 0 && t("common.folder", { count: folders }), files > 0 && t("common.file", { count: files })]
    .filter(Boolean)
    .join(", ");
  // The first items, the first one in front.
  const pile = entries.slice(0, 3);
  return (
    <div className={`grid h-full place-items-center px-6 text-center ${bare ? "" : "glass"}`}>
      <div>
        <div className="relative mx-auto h-28 w-28">
          {pile
            .map((e, i) => (
              <div
                key={e.path}
                className="absolute inset-0 grid place-items-center"
                style={{ transform: `translate(${i * 10}px, ${-i * 8}px) rotate(${i * 5}deg)`, opacity: 1 - i * 0.22 }}
              >
                <EntryGlyph entry={e} size={80} />
              </div>
            ))
            .reverse()}
        </div>
        <p className="mt-5 text-[0.95rem] font-semibold">{t("common.item", { count: entries.length })}</p>
        <p className="mt-0.5 text-[0.72rem] text-muted">
          {counts}
          {files > 0 && ` \u00b7 ${bytesText(size)}`}
        </p>
      </div>
    </div>
  );
}

/** Preview with the file's details and actions (list view side pane, last column). */
export function PreviewPane({
  entry,
  platform,
  bare = false,
  folder,
  onQuickLook,
}: {
  entry: Entry | null;
  platform: "macOS" | "Linux";
  bare?: boolean;
  /** Shown when nothing is selected: the folder in view. */
  folder?: { name: string; path: string; items: number };
  /** The Quick Look button (Space): the file over the whole window. */
  onQuickLook?: () => void;
}) {
  if (!entry) {
    return (
      <div className={`grid h-full place-items-center px-6 text-center ${bare ? "" : "glass"}`}>
        <div>
          {folder && <FolderArt entry={folderEntry(folder.path, folder.name)} size={144} />}
          {folder && (
            <>
              <p className="mt-4 text-[0.95rem] font-semibold">{folder.name}</p>
              <p className="mt-0.5 text-[0.72rem] text-muted">
                {t("common.item", { count: folder.items })}
              </p>
            </>
          )}
          <p className="mt-3 text-[0.7rem] text-dim">{t("views.selectToPreview")}</p>
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
          <Meta label={t("views.modified")} value={dateLabel(entry.modified)} />
          <Meta label={t("views.where")} value={entry.path.slice(0, entry.path.lastIndexOf("/")) || "/"} />
          {entry.kind === "symlink" && <Meta label={t("views.type")} value={t("views.symlink")} />}
        </div>
      </div>
      <Actions entry={entry} onQuickLook={onQuickLook} />
    </div>
  );
}

function Actions({ entry, onQuickLook }: { entry: Entry; onQuickLook?: () => void }) {
  return (
    <div className="mt-3 flex flex-none gap-2">
      <button className="btn btn-primary flex-1" onClick={() => api.openPath(entry.path)}>
        <ExternalLink className="h-3.5 w-3.5" />
        {t("common.open")}
      </button>
      {onQuickLook && (
        <button className="btn btn-ghost" onClick={onQuickLook} title={t("views.quickLookTitle")}>
          <Eye className="h-3.5 w-3.5" />
          {t("views.quickLook")}
        </button>
      )}
    </div>
  );
}

/** Space-bar preview over the whole window, as Finder's Quick Look. */
export function QuickLook({ entry, platform, onClose }: { entry: Entry; platform: "macOS" | "Linux"; onClose: () => void }) {
  return (
    <div className="fade-in fixed inset-0 z-40 grid place-items-center bg-black/35 p-10 backdrop-blur-md" onMouseDown={onClose}>
      <div className="glass sheet flex h-full max-h-[52rem] w-full max-w-[72rem] flex-col p-5" onMouseDown={(e) => e.stopPropagation()}>
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
            {t("common.open")}
          </button>
          <button aria-label={t("common.close")} onClick={onClose} className="grid h-9 w-9 place-items-center rounded-full bg-white/8 text-muted hover:bg-white/14 hover:text-ink">
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
