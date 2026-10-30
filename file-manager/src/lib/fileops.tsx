import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { DeleteDialog, NewFolderDialog, TrashDialog } from "../components/Dialog";
import { api, baseName, CANCELLED, errorText, parentOf, type Entry, type MetaEdit } from "./api";
import { rebase } from "./nav";
import { newTaskId, titleFor, withSample, type OpTask, type TaskKind } from "./tasks";
import { t } from "../i18n";

/** How often the progress of the operations under way is read. */
const POLL_MS = 250;
/** How long a finished operation stays in the list ("Done"). */
const DONE_LINGER_MS = 4000;
/** Why a delete on the drive is for good on Linux (also the backend's words). */
const driveNoTrash = () => t("fileops.driveNoTrash");

/** What Copy or Cut put aside; Paste copies it, or moves it when `cut`. */
export interface Clip {
  paths: string[];
  cut: boolean;
}

/** Paths waiting to be moved by a Paste: the views draw them faded. */
const CutContext = createContext<ReadonlySet<string>>(new Set());
export const CutProvider = CutContext.Provider;
export const useIsCut = (path: string) => useContext(CutContext).has(path);

/** The item whose name is being edited in place, and how the edit ends. */
export interface Renaming {
  path: string;
  /** Rejects with the reason when the name is refused. */
  commit: (name: string) => Promise<void>;
  cancel: (reason?: string) => void;
}

const RenameContext = createContext<Renaming | null>(null);
export const RenameProvider = RenameContext.Provider;
/** The edit in progress when it is `path`'s name: the views then show a field. */
export function useRename(path: string): Renaming | null {
  const renaming = useContext(RenameContext);
  return renaming?.path === path ? renaming : null;
}

type Dialog =
  | { kind: "folder"; dir: string }
  | { kind: "trash"; paths: string[] }
  | { kind: "delete"; paths: string[]; reason: string }
  | { kind: "empty"; count: number };

/**
 * The context menu's operations: a clipboard of paths (this app's own: Finder
 * and the file managers of Linux do not see it), paste, rename, move to the
 * Trash and new folder. The long ones run as tasks, side by side, each with
 * its progress (read from the backend while any runs) and a cancel; a
 * finished one stays a few seconds, a failed or cancelled one until it is
 * dismissed. The callbacks let the window refresh and select what changed.
 */
export function useFileOps({
  toast,
  onChanged,
  onLanded,
  onRemoved,
  onRenamed,
}: {
  toast: (tone: "ok" | "error", text: string) => void;
  /** Something changed on disk: re-read the folders in view. */
  onChanged: () => void;
  /** Where pasted or created items landed (select them). */
  onLanded: (paths: string[]) => void;
  /** Items that are gone (drop them from the selection and the results). */
  onRemoved: (paths: string[]) => void;
  /** `from` is now `to` (the selection and the history follow it). */
  onRenamed: (from: string, to: string) => void;
}) {
  const [clip, setClip] = useState<Clip | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [tasks, setTasks] = useState<OpTask[]>([]);
  // Tasks shown in the details dialog stay listed after they end, until it closes.
  const viewing = useRef<ReadonlySet<number>>(new Set());

  const cutPaths = useMemo<ReadonlySet<string>>(() => new Set(clip?.cut ? clip.paths : []), [clip]);

  const copy = useCallback((entries: Entry[]) => setClip({ paths: entries.map((e) => e.path), cut: false }), []);
  const cut = useCallback((entries: Entry[]) => setClip({ paths: entries.map((e) => e.path), cut: true }), []);

  const patch = (id: number, change: Partial<OpTask>) => setTasks((ts) => ts.map((t) => (t.id === id ? { ...t, ...change } : t)));
  const dismissTask = useCallback((id: number) => setTasks((ts) => ts.filter((t) => t.id !== id || t.state === "running")), []);
  const cancelTask = useCallback((id: number) => {
    setTasks((ts) => ts.map((t) => (t.id === id && t.state === "running" ? { ...t, cancelling: true } : t)));
    api.cancelTask(id).catch(() => {});
  }, []);
  /** The details dialog shows these: they stay listed while it does. */
  const viewTasks = useCallback((ids: number[]) => {
    viewing.current = new Set(ids);
    if (ids.length === 0) setTasks((ts) => ts.filter((t) => t.state === "running" || t.state !== "done" || t.ended == null || Date.now() - t.ended < DONE_LINGER_MS));
  }, []);

  /** Runs an operation as a task with the id the backend reports its progress under. */
  /** Runs an operation as a task; resolves with whether it finished. */
  const run = async (spec: { kind: TaskKind; sources: string[]; dest: string | null; title?: string }, work: (task: number) => Promise<void>): Promise<boolean> => {
    const id = newTaskId();
    const title = spec.title ?? titleFor(spec.kind, spec.sources.length, spec.sources.length === 1 ? baseName(spec.sources[0]) : undefined);
    setTasks((ts) => [
      ...ts,
      { id, kind: spec.kind, title, sources: spec.sources, dest: spec.dest, state: "running", cancelling: false, progress: null, error: null, started: Date.now(), ended: null, samples: [] },
    ]);
    try {
      await work(id);
      patch(id, { state: "done", ended: Date.now() });
      window.setTimeout(() => {
        if (!viewing.current.has(id)) setTasks((ts) => ts.filter((t) => t.id !== id));
      }, DONE_LINGER_MS);
      return true;
    } catch (e) {
      const text = errorText(e);
      patch(id, { state: text === CANCELLED ? "cancelled" : "failed", error: text === CANCELLED ? null : text, ended: Date.now() });
      return false;
    } finally {
      onChanged();
    }
  };

  // While any runs: its progress, and the samples its speed comes from.
  const running = tasks.some((t) => t.state === "running");
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => {
      api
        .taskProgress()
        .then((all) => {
          const now = Date.now();
          setTasks((ts) =>
            ts.map((t) => {
              const p = all[String(t.id)];
              return p && t.state === "running" ? { ...t, progress: p, samples: withSample(t.samples, now, p) } : t;
            }),
          );
        })
        .catch(() => {});
    }, POLL_MS);
    return () => window.clearInterval(timer);
  }, [running]);

  const paste = (dir: string) => {
    if (!clip) return;
    const { paths, cut: moving } = clip;
    void run({ kind: moving ? "move" : "copy", sources: paths, dest: dir }, async (task) => {
      const landed = await api.paste(paths, dir, moving, task);
      // Moved items are no longer where Cut found them; a copy can be pasted again.
      if (moving) setClip(null);
      onLanded(landed);
    });
  };

  /** Items dropped on `dir`, moved or copied as the keys held and the volumes decide. */
  const drop = (paths: string[], dir: string) => {
    api.dropOperation(paths, dir).then(
      (op) => {
        const moving = op === "move";
        // Dropped back where they are: nothing to do.
        if (moving && paths.every((p) => parentOf(p) === dir)) return;
        void run({ kind: moving ? "move" : "copy", sources: paths, dest: dir }, async (task) => {
          const landed = await api.paste(paths, dir, moving, task);
          if (moving) forget(paths.filter((p) => !landed.includes(p)));
          onLanded(landed);
        });
      },
      (e) => toast("error", errorText(e)),
    );
  };

  const forget = (paths: string[]) => {
    setClip((c) => (c && c.paths.some((p) => paths.includes(p)) ? null : c));
    onRemoved(paths);
  };

  /** Moves `paths` to the Trash now (a drop on the sidebar's Trash: the gesture already says so). */
  const trashNow = (paths: string[]) => {
    void run({ kind: "trash", sources: paths, dest: null }, async (task) => {
      const result = await api.trash(paths, task);
      if (result.outcome === "no_trash") {
        forget(paths.filter((p) => !result.remaining.includes(p)));
        setDialog({ kind: "delete", paths: result.remaining, reason: result.reason });
      } else forget(paths);
    });
  };

  /** Asks, then moves `paths` to the Trash; where there is none (`noTrash`:
   * the drive on Linux), asks before deleting them for good. */
  const trash = (paths: string[], noTrash = false) =>
    setDialog(noTrash ? { kind: "delete", paths, reason: driveNoTrash() } : { kind: "trash", paths });

  /** Items already in the Trash: deleting them is for good, after asking. */
  const deleteForGood = (paths: string[]) =>
    setDialog({ kind: "delete", paths, reason: t("fileops.trashNoPutBack") });

  const deleteNow = (paths: string[]) => {
    setDialog(null);
    void run({ kind: "delete", sources: paths, dest: null }, async (task) => {
      try {
        await api.deletePermanently(paths, task);
      } finally {
        // What went before a cancel or an error is gone too.
        const kinds = await api.pathKinds(paths).catch(() => paths.map(() => null));
        forget(paths.filter((_, i) => kinds[i] == null));
      }
    });
  };

  /** Asks, then deletes everything in the Trash. */
  const emptyTrash = (count: number) => setDialog({ kind: "empty", count });

  /** Copies beside themselves (`name copy`), as Finder's Duplicate. */
  const duplicate = (paths: string[]) => {
    const dir = parentOf(paths[0] ?? "");
    if (!dir) return;
    void run({ kind: "duplicate", sources: paths, dest: dir }, async (task) => onLanded(await api.paste(paths, dir, false, task)));
  };

  /** Zips the items beside them (`name.zip`, or `Archive.zip` for several). */
  const compress = (paths: string[]) => {
    void run({ kind: "compress", sources: paths, dest: parentOf(paths[0] ?? "") }, async (task) => onLanded([await api.compress(paths, task)]));
  };

  /** Extracts an archive beside it, without ever merging into what is there. */
  const extract = (path: string) => {
    void run({ kind: "extract", sources: [path], dest: parentOf(path) }, async (task) => onLanded(await api.extract(path, task)));
  };

  /** Items of the Trash back where they were deleted from (Linux). */
  const putBack = (paths: string[]) => {
    void run({ kind: "put-back", sources: paths, dest: null }, async () => {
      for (const p of paths) {
        const to = await api.putBack(p);
        forget([p]);
        toast("ok", t("fileops.putBack", { name: baseName(to), folder: baseName(parentOf(to) ?? "/") }));
      }
    });
  };

  const newFolder = (dir: string) => setDialog({ kind: "folder", dir });

  /** Changes permissions (Properties); resolves with whether it finished. */
  const setMode = (paths: string[], change: { set: number; clear: number }, recursive: boolean) =>
    run({ kind: "permissions", sources: paths, dest: null }, async (task) => {
      await api.setMode(paths, change, recursive, task);
    });

  /** Changes image metadata (Properties); resolves with whether it finished. */
  const writeMeta = (paths: string[], edit: MetaEdit) =>
    run({ kind: "metadata", sources: paths, dest: null }, async (task) => {
      await api.writeImageMeta(paths, edit, task);
    });

  /** Turns `entry`'s name into a field (the views draw it). */
  const rename = (entry: Entry) => setRenaming(entry.path);

  const commitRename = async (from: string, name: string) => {
    // An empty or unchanged name keeps the old one, as in Finder.
    if (name.trim() === "" || name.trim() === baseName(from)) return setRenaming(null);
    const to = await api.rename(from, name);
    setRenaming(null);
    setClip((c) => c && { ...c, paths: c.paths.map((p) => rebase(p, from, to)) });
    onRenamed(from, to);
    onChanged();
  };

  const renamingNow: Renaming | null =
    renaming == null
      ? null
      : {
          path: renaming,
          commit: (name) => commitRename(renaming, name),
          cancel: (reason) => {
            setRenaming(null);
            if (reason) toast("error", reason);
          },
        };

  const close = useCallback(() => setDialog(null), []);
  const dialogs =
    dialog?.kind === "folder" ? (
      <NewFolderDialog
        where={baseName(dialog.dir)}
        onCancel={close}
        onCreate={async (name) => {
          // Rejects with the reason, which the dialog shows; it stays open to fix the name.
          const path = await api.newFolder(dialog.dir, name);
          setDialog(null);
          onChanged();
          onLanded([path]);
        }}
      />
    ) : dialog?.kind === "trash" ? (
      <TrashDialog
        count={dialog.paths.length}
        title={
          dialog.paths.length === 1
            ? t("fileops.trashOne", { name: baseName(dialog.paths[0]) })
            : t("fileops.trashMany", { count: dialog.paths.length })
        }
        onCancel={close}
        onConfirm={() => {
          setDialog(null);
          trashNow(dialog.paths);
        }}
      />
    ) : dialog?.kind === "delete" ? (
      <DeleteDialog
        title={
          dialog.paths.length === 1
            ? t("fileops.deleteOne", { name: baseName(dialog.paths[0]) })
            : t("fileops.deleteMany", { count: dialog.paths.length })
        }
        reason={dialog.reason}
        onCancel={close}
        onConfirm={() => deleteNow(dialog.paths)}
      />
    ) : dialog?.kind === "empty" ? (
      <DeleteDialog
        title={t("fileops.emptyTitle")}
        reason={t("fileops.emptyReason", { count: dialog.count })}
        confirm={t("fileops.emptyConfirm")}
        onCancel={close}
        onConfirm={() => {
          setDialog(null);
          void run({ kind: "empty", sources: [], dest: null }, async (task) => {
            await api.emptyTrash(task);
          });
        }}
      />
    ) : null;

  return {
    clip,
    cutPaths,
    tasks,
    cancelTask,
    dismissTask,
    viewTasks,
    copy,
    cut,
    paste,
    drop,
    rename,
    renaming: renamingNow,
    trash,
    trashNow,
    duplicate,
    compress,
    extract,
    deleteForGood,
    emptyTrash,
    putBack,
    newFolder,
    setMode,
    writeMeta,
    dialogs,
    dialogOpen: dialog != null,
  };
}

/** The window's file operations, shared by its panes (one clipboard, the tasks under way). */
export type FileOps = ReturnType<typeof useFileOps>;
