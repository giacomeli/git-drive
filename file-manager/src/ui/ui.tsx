// Copied from control-center/src/components/ui.tsx (file manager POC, decision 2): keep the two
// in sync until the design system moves to a shared package.
import { createContext, useCallback, useContext, useState, type ReactNode } from "react";
import { CircleAlert, CircleCheck, X } from "lucide-react";

export function Card({
  className = "",
  children,
  title,
  subtitle,
  action,
}: {
  className?: string;
  children: ReactNode;
  title?: ReactNode;
  subtitle?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <section className={`glass p-5 ${className}`}>
      {(title || action) && (
        <header className="mb-4 flex items-start justify-between gap-3">
          <div className="min-w-0">
            {title && <h2 className="text-[0.95rem] font-semibold text-ink">{title}</h2>}
            {subtitle && <p className="mt-0.5 text-xs text-muted">{subtitle}</p>}
          </div>
          {action}
        </header>
      )}
      {children}
    </section>
  );
}

export type Tone = "mint" | "amber" | "coral" | "cyan" | "violet" | "dim";

const TONE: Record<Tone, string> = {
  mint: "#46e3a4",
  amber: "#ffb547",
  coral: "#ff5c7a",
  cyan: "#3fd0f0",
  violet: "#8f6bff",
  dim: "#6f7499",
};

export function toneColor(tone: Tone): string {
  return TONE[tone];
}

export function StatusDot({ tone, pulse = false }: { tone: Tone; pulse?: boolean }) {
  return (
    <span
      className={`dot ${pulse ? "pulse" : ""}`}
      style={{ background: TONE[tone], color: `${TONE[tone]}66`, boxShadow: `0 0 10px ${TONE[tone]}88` }}
    />
  );
}

export function Pill({ tone, children }: { tone: Tone; children: ReactNode }) {
  return (
    <span
      className="inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-[0.7rem] font-medium"
      style={{ background: `${TONE[tone]}1f`, color: TONE[tone], border: `1px solid ${TONE[tone]}40` }}
    >
      <StatusDot tone={tone} />
      {children}
    </span>
  );
}

export function Toggle({
  checked,
  onChange,
  disabled = false,
}: {
  checked: boolean;
  onChange: (value: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className="relative h-6 w-11 flex-none rounded-full transition-all duration-200 disabled:opacity-40"
      style={{
        background: checked ? "var(--accent)" : "rgb(255 255 255 / 0.12)",
        boxShadow: checked ? "0 6px 16px -6px rgb(var(--a3-rgb) / 0.8)" : "inset 0 1px 2px rgb(0 0 0 / 0.3)",
      }}
    >
      <span
        className="absolute top-0.5 h-5 w-5 rounded-full bg-paper shadow transition-all duration-200"
        style={{ left: checked ? "1.375rem" : "0.125rem" }}
      />
    </button>
  );
}

/** Compact segmented control (log levels). */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
  labels,
}: {
  options: readonly T[];
  value: T;
  onChange: (value: T) => void;
  labels?: Partial<Record<T, string>>;
}) {
  return (
    <div className="inline-flex rounded-full border border-line bg-black/20 p-0.5">
      {options.map((opt) => (
        <button
          key={opt}
          type="button"
          onClick={() => onChange(opt)}
          className={`rounded-full px-2.5 py-1 text-[0.68rem] font-medium capitalize transition-all ${
            opt === value ? "text-paper shadow" : "text-dim hover:text-muted"
          }`}
          style={opt === value ? { background: "var(--accent)" } : undefined}
        >
          {labels?.[opt] ?? opt}
        </button>
      ))}
    </div>
  );
}

// ---- Toasts ----

interface Toast {
  id: number;
  tone: "ok" | "error";
  text: string;
}

const ToastContext = createContext<(tone: Toast["tone"], text: string) => void>(() => {});

export function useToast() {
  return useContext(ToastContext);
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const dismiss = useCallback((id: number) => setToasts((all) => all.filter((t) => t.id !== id)), []);
  const push = useCallback(
    (tone: Toast["tone"], text: string) => {
      const id = Date.now() + Math.random();
      setToasts((all) => [...all.slice(-3), { id, tone, text }]);
      window.setTimeout(() => dismiss(id), tone === "error" ? 9000 : 4000);
    },
    [dismiss],
  );
  return (
    <ToastContext.Provider value={push}>
      {children}
      <div className="pointer-events-none fixed bottom-5 right-5 z-50 flex w-96 flex-col gap-2">
        {toasts.map((t) => (
          <div key={t.id} className="glass glass-strong fade-in pointer-events-auto flex items-start gap-3 px-4 py-3">
            {t.tone === "ok" ? (
              <CircleCheck className="mt-0.5 h-4 w-4 flex-none text-mint" />
            ) : (
              <CircleAlert className="mt-0.5 h-4 w-4 flex-none text-coral" />
            )}
            <p className="selectable min-w-0 flex-1 text-xs leading-relaxed text-ink">{t.text}</p>
            <button onClick={() => dismiss(t.id)} className="text-dim hover:text-ink" aria-label="Dismiss">
              <X className="h-3.5 w-3.5" />
            </button>
          </div>
        ))}
      </div>
    </ToastContext.Provider>
  );
}
