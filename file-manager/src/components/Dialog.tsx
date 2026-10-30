import { FolderPlus, Trash2 } from "lucide-react";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { errorText } from "../lib/api";
import { t } from "../i18n";

/** A modal panel over a dimmed window; Esc or a click outside cancels. */
export function Modal({ children, onCancel, className = "w-[26rem]" }: { children: ReactNode; onCancel: () => void; className?: string }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onCancel();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onCancel]);
  return createPortal(
    <div
      className="fixed inset-0 z-40 grid place-items-center bg-black/30 p-6"
      onMouseDown={onCancel}
      onContextMenu={(e) => e.preventDefault()}
    >
      <div role="dialog" aria-modal className={`float-panel dialog-panel max-w-full p-5 ${className}`} onMouseDown={(e) => e.stopPropagation()}>
        {children}
      </div>
    </div>,
    document.body,
  );
}

export function Heading({ icon, title, subtitle }: { icon: ReactNode; title: ReactNode; subtitle?: ReactNode }) {
  return (
    <div className="flex items-start gap-3">
      <span className="mt-0.5 grid h-9 w-9 flex-none place-items-center rounded-xl bg-white/8 ring-1 ring-white/10">{icon}</span>
      <div className="min-w-0">
        <h2 className="break-words text-[0.95rem] font-semibold leading-snug text-ink">{title}</h2>
        {subtitle && <p className="mt-0.5 break-words text-[0.74rem] text-muted">{subtitle}</p>}
      </div>
    </div>
  );
}


/** Asks for the new folder's name; `onCreate` rejects with the reason to show. */
export function NewFolderDialog({
  where,
  onCreate,
  onCancel,
}: {
  where: string;
  onCreate: (name: string) => Promise<void>;
  onCancel: () => void;
}) {
  // Finder's default name for a new folder, in the app's language.
  const [name, setName] = useState(() => t("fileops.untitled"));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);

  const submit = () => {
    if (busy || name.trim() === "") return;
    setBusy(true);
    onCreate(name).catch((e) => {
      setError(errorText(e));
      setBusy(false);
      input.current?.focus();
    });
  };

  return (
    <Modal onCancel={onCancel}>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <Heading icon={<FolderPlus className="h-4.5 w-4.5 accent-fg" />} title={t("fileops.newFolder")} subtitle={t("fileops.newFolderIn", { where })} />
        <input
          ref={input}
          className="field mt-4"
          value={name}
          spellCheck={false}
          aria-label={t("fileops.folderName")}
          onChange={(e) => {
            setName(e.target.value);
            setError(null);
          }}
        />
        <p className={`mt-2 min-h-[1rem] text-[0.72rem] ${error ? "text-coral" : "text-dim"}`}>{error ?? ""}</p>
        <div className="mt-3 flex justify-end gap-2">
          <button type="button" className="btn btn-ghost" onClick={onCancel}>
            {t("common.cancel")}
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy || name.trim() === ""}>
            {t("fileops.create")}
          </button>
        </div>
      </form>
    </Modal>
  );
}

/** Asks before deleting for good (where no Trash can take the item, inside
 * the Trash, emptying it). */
export function DeleteDialog({
  title,
  reason,
  confirm = t("fileops.delete"),
  onConfirm,
  onCancel,
}: {
  title: string;
  reason: string;
  confirm?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Modal onCancel={onCancel}>
      <Heading icon={<Trash2 className="h-4.5 w-4.5 text-coral" />} title={title} subtitle={reason} />
      <p className="mt-3 text-[0.78rem] text-ink/90">{t("fileops.cantUndo")}</p>
      <div className="mt-5 flex justify-end gap-2">
        {/* Cancel takes Enter: a delete must be a deliberate click. */}
        <button type="button" className="btn btn-ghost" onClick={onCancel} autoFocus>
          {t("common.cancel")}
        </button>
        <button type="button" className="btn btn-danger" onClick={onConfirm}>
          {confirm}
        </button>
      </div>
    </Modal>
  );
}

/** Asks before moving items to the Trash (from the menu or the keyboard).
 * Return confirms: the items can come back from the Trash. */
export function TrashDialog({ title, count, onConfirm, onCancel }: { title: string; count: number; onConfirm: () => void; onCancel: () => void }) {
  return (
    <Modal onCancel={onCancel}>
      <Heading
        icon={<Trash2 className="h-4.5 w-4.5 text-muted" />}
        title={title}
        subtitle={count === 1 ? t("fileops.staysOne") : t("fileops.staysMany")}
      />
      <div className="mt-5 flex justify-end gap-2">
        <button type="button" className="btn btn-ghost" onClick={onCancel}>
          {t("common.cancel")}
        </button>
        <button type="button" className="btn btn-primary" onClick={onConfirm} autoFocus>
          {t("fileops.moveToTrash")}
        </button>
      </div>
    </Modal>
  );
}

