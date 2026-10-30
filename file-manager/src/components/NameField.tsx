import { CircleAlert } from "lucide-react";
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ChangeEvent,
  type CSSProperties,
  type KeyboardEvent,
  type RefObject,
  type SyntheticEvent,
} from "react";
import { t } from "../i18n";
import { createPortal } from "react-dom";
import { errorText, type Entry } from "../lib/api";
import { useRename } from "../lib/fileops";

/** An item's name, or the field that edits it while it is being renamed. */
export function EntryName({
  entry,
  className,
  style,
  fieldClassName,
  fieldStyle,
  multiline,
}: {
  entry: Entry;
  className?: string;
  style?: CSSProperties;
  fieldClassName?: string;
  fieldStyle?: CSSProperties;
  multiline?: boolean;
}) {
  const renaming = useRename(entry.path);
  if (!renaming) {
    return (
      <span className={className} style={style}>
        {entry.name}
      </span>
    );
  }
  return (
    <NameField
      entry={entry}
      multiline={multiline}
      className={fieldClassName}
      style={fieldStyle}
      onCommit={renaming.commit}
      onCancel={renaming.cancel}
    />
  );
}

/**
 * Finder's rename in place: the name turns into a field, selected up to its
 * extension. Enter saves, Esc cancels, and so does a click anywhere else or
 * Tab (saves). A refused name keeps the field open with the reason below
 * it; leaving it then drops the edit. Switching to another app keeps it open.
 */
export function NameField({
  entry,
  multiline = false,
  className = "",
  style,
  onCommit,
  onCancel,
}: {
  entry: Entry;
  /** The icon view: centered, wrapping like the name it replaces. */
  multiline?: boolean;
  className?: string;
  style?: CSSProperties;
  /** Rejects with the reason when the name is refused. */
  onCommit: (name: string) => Promise<void>;
  /** Ends the edit; `reason` says why the name was dropped on the way out. */
  onCancel: (reason?: string) => void;
}) {
  const ref = useRef<HTMLInputElement | HTMLTextAreaElement | null>(null);
  const [value, setValue] = useState(entry.name);
  const [error, setError] = useState<string | null>(null);
  // Set once the name is on its way (or the edit was cancelled): a blur that
  // follows must not save again.
  const saving = useRef(false);

  // Once: a refresh of the folder hands a new `entry` with the same name.
  const { name, is_dir } = entry;
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus();
    const dot = name.lastIndexOf(".");
    el.setSelectionRange(0, !is_dir && dot > 0 ? dot : name.length);
  }, [name, is_dir]);

  // The icon view's field grows a line at a time, as the name wraps.
  useLayoutEffect(() => {
    const el = ref.current;
    if (!multiline || !el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [value, multiline]);

  const save = (leaving: boolean) => {
    if (saving.current) return;
    saving.current = true;
    // Leaving after a refusal: the reason is on screen already.
    if (leaving && error) return onCancel();
    onCommit(value).catch((e) => {
      if (leaving) return onCancel(errorText(e));
      saving.current = false;
      setError(errorText(e));
      ref.current?.focus();
    });
  };
  const latestSave = useRef(save);
  latestSave.current = save;

  useEffect(() => {
    // A click elsewhere saves before anything else sees it: the click may
    // select another item or navigate away, unmounting this field.
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) latestSave.current(true);
    };
    // Back from another app: the edit goes on where it was.
    const onFocus = () => ref.current?.focus();
    window.addEventListener("mousedown", onDown, true);
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  // The item's own handlers (select, open, its menu) stay out of the field.
  const stop = (e: SyntheticEvent) => e.stopPropagation();
  const field = {
    className: `name-field ${error ? "invalid" : ""} ${className}`,
    style,
    value,
    spellCheck: false,
    autoCorrect: "off",
    autoCapitalize: "off",
    "aria-label": t("names.name"),
    "aria-invalid": error != null,
    onChange: (e: ChangeEvent<HTMLInputElement | HTMLTextAreaElement>) => {
      // A name has no line breaks, even pasted ones.
      setValue(e.target.value.replace(/[\r\n]+/g, " "));
      setError(null);
    },
    onKeyDown: (e: KeyboardEvent<HTMLElement>) => {
      // The window's shortcuts (arrows, Enter, Cmd+C) stay out of the field.
      e.stopPropagation();
      if (e.nativeEvent.isComposing) return;
      if (e.key === "Enter") {
        e.preventDefault();
        save(false);
      } else if (e.key === "Escape") {
        e.preventDefault();
        saving.current = true;
        onCancel();
      }
    },
    onBlur: () => {
      if (document.hasFocus()) save(true);
    },
    onMouseDown: stop,
    onClick: stop,
    onDoubleClick: stop,
    onContextMenu: stop,
  };

  return (
    <>
      {multiline ? (
        <textarea ref={ref as RefObject<HTMLTextAreaElement>} rows={1} {...field} />
      ) : (
        <input ref={ref as RefObject<HTMLInputElement>} {...field} />
      )}
      {error && <FieldError anchor={ref.current} text={error} centered={multiline} onGone={() => setError(null)} />}
    </>
  );
}

/** The reason a name was refused, just below the field. */
function FieldError({
  anchor,
  text,
  centered,
  onGone,
}: {
  anchor: HTMLElement | null;
  text: string;
  centered: boolean;
  onGone: () => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el || !anchor) return;
    const a = anchor.getBoundingClientRect();
    const w = el.offsetWidth;
    const h = el.offsetHeight;
    const left = centered ? a.left + a.width / 2 - w / 2 : a.left;
    let top = a.bottom + 6;
    if (top + h > window.innerHeight - 12) top = Math.max(12, a.top - 6 - h);
    setPos({ left: Math.min(Math.max(12, left), window.innerWidth - 12 - w), top });
  }, [anchor, text, centered]);

  // The field moved with the scroll; the tip would point at nothing.
  useEffect(() => {
    window.addEventListener("scroll", onGone, true);
    return () => window.removeEventListener("scroll", onGone, true);
  }, [onGone]);

  return createPortal(
    <div
      ref={ref}
      role="alert"
      className="float-panel pointer-events-none fixed z-50 flex max-w-[20rem] items-start gap-2 px-3 py-2 text-[0.72rem] leading-[1.05rem] text-ink"
      style={{ left: pos?.left ?? 0, top: pos?.top ?? 0, visibility: pos ? "visible" : "hidden" }}
    >
      <CircleAlert className="mt-px h-3.5 w-3.5 flex-none text-coral" strokeWidth={2} />
      <span>{text}</span>
    </div>,
    document.body,
  );
}
