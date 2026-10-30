import { Check, Copy, CopyPlus, Eye, FileArchive, FolderInput, KeyRound, PackageOpen, Tags, Trash2, Undo2, X, type LucideIcon } from "lucide-react";
import { baseName } from "../lib/api";
import { fraction, progressText, rate, secondsLeft, tildePath, type OpTask, type TaskKind } from "../lib/tasks";
import { bytesText, duration } from "../ui/format";
import { Heading, Modal } from "./Dialog";
import { num, t, type Key } from "../i18n";

const KIND_ICON: Record<TaskKind, LucideIcon> = {
  copy: Copy,
  move: FolderInput,
  duplicate: CopyPlus,
  trash: Trash2,
  delete: Trash2,
  compress: FileArchive,
  extract: PackageOpen,
  empty: Trash2,
  "put-back": Undo2,
  permissions: KeyRound,
  metadata: Tags,
};

/** Status colors never follow the accent: done is mint, a failure coral. */
function toneOf(task: OpTask): string | undefined {
  if (task.state === "done") return "var(--color-mint)";
  if (task.state === "failed") return "var(--color-coral)";
  return undefined;
}

/** The bar: the accent up to what is done, or a sweep while nothing tells how far. */
function Bar({ task, className = "" }: { task: OpTask; className?: string }) {
  const f = fraction(task);
  const ended = task.state !== "running";
  return (
    <div className={`relative h-1 overflow-hidden rounded-full bg-white/10 ${className}`}>
      {f == null && !ended ? (
        <span className="task-sweep absolute inset-y-0 w-1/3 rounded-full" />
      ) : (
        <span
          className="absolute inset-y-0 left-0 rounded-full transition-[width] duration-300"
          style={{
            width: `${Math.round((f ?? 0) * 100)}%`,
            background: toneOf(task) ?? (ended ? "rgb(255 255 255 / 0.3)" : "linear-gradient(90deg, var(--a1), var(--a3))"),
          }}
        />
      )}
    </div>
  );
}

function SmallButton({ label, onClick, disabled, children }: { label: string; onClick: () => void; disabled?: boolean; children: React.ReactNode }) {
  return (
    <button
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
      className="-mr-1 grid h-6 w-6 flex-none place-items-center rounded-full text-dim transition-colors hover:bg-white/10 hover:text-ink disabled:opacity-40"
    >
      {children}
    </button>
  );
}

/** Where the items go, or what the task is on. */
function subtitleOf(task: OpTask): string {
  if (task.dest && task.kind !== "compress" && task.kind !== "extract") return t("tasks.toFolder", { name: baseName(task.dest) });
  const current = task.progress?.current;
  if (task.state === "running" && current) return baseName(current);
  if (task.dest) return t("tasks.inFolder", { name: baseName(task.dest) });
  return task.sources.length === 1 ? baseName(task.sources[0]) : "";
}

/** One operation in the sidebar: what it does, its bar, how much is left; the eye opens its details. */
function TaskCard({ task, onCancel, onDismiss, onDetails }: { task: OpTask; onCancel: () => void; onDismiss: () => void; onDetails: () => void }) {
  const Icon = task.state === "done" ? Check : KIND_ICON[task.kind];
  const running = task.state === "running";
  const sub = subtitleOf(task);
  const tone = toneOf(task);
  return (
    <div data-task={task.id} className="task-card rounded-2xl px-3 pb-2 pt-2.5">
      <div className="flex items-start gap-2.5">
        <Icon className={`mt-0.5 h-3.5 w-3.5 flex-none ${tone ? "" : "accent-fg"}`} style={tone ? { color: tone } : undefined} />
        <div className="min-w-0 flex-1">
          <p className="truncate text-[0.74rem] font-medium leading-tight text-ink" title={task.title}>
            {task.title}
          </p>
          {sub && <p className="mt-0.5 truncate text-[0.66rem] leading-tight text-dim">{sub}</p>}
        </div>
      </div>
      <Bar task={task} className="mt-2" />
      {/* The buttons share the last line: the title above keeps the card's whole width. */}
      <div className="mt-1 flex items-center gap-1">
        <p className={`min-w-0 flex-1 truncate text-[0.64rem] ${task.state === "failed" ? "text-coral" : "text-muted"}`} title={progressText(task)}>
          {progressText(task)}
        </p>
        <SmallButton label={t("tasks.showDetails")} onClick={onDetails}>
          <Eye className="h-3.5 w-3.5" />
        </SmallButton>
        {running ? (
          <SmallButton label={task.cancelling ? t("tasks.cancellingLabel") : t("common.cancel")} onClick={onCancel} disabled={task.cancelling}>
            <X className="h-3.5 w-3.5" />
          </SmallButton>
        ) : (
          <SmallButton label={t("tasks.dismiss")} onClick={onDismiss}>
            <X className="h-3.5 w-3.5" />
          </SmallButton>
        )}
      </div>
    </div>
  );
}

/** A ring with what is done of the operations under way (the sidebar collapsed). */
function Ring({ tasks, onClick }: { tasks: OpTask[]; onClick: () => void }) {
  const running = tasks.filter((t) => t.state === "running");
  const known = running.map(fraction).filter((f): f is number => f != null);
  const f = running.length === 0 ? 1 : known.length === running.length ? known.reduce((a, b) => a + b, 0) / known.length : null;
  const failed = tasks.some((t) => t.state === "failed");
  const color = failed ? "var(--color-coral)" : running.length === 0 ? "var(--color-mint)" : "var(--a2)";
  const r = 14;
  const c = 2 * Math.PI * r;
  return (
    <button
      aria-label={t("tasks.ringLabel", { count: tasks.length })}
      title={running.length > 0 ? t("tasks.underWayCount", { count: running.length }) : t("tasks.operations")}
      onClick={onClick}
      className="relative mx-auto grid h-10 w-10 place-items-center rounded-full bg-white/6 ring-1 ring-white/10 transition-colors hover:bg-white/12"
    >
      <svg viewBox="0 0 36 36" className={`absolute inset-0 h-10 w-10 -rotate-90 ${f == null ? "spin" : ""}`}>
        <circle cx="18" cy="18" r={r} fill="none" stroke="rgb(255 255 255 / 0.12)" strokeWidth="3" />
        <circle
          cx="18"
          cy="18"
          r={r}
          fill="none"
          stroke={color}
          strokeWidth="3"
          strokeLinecap="round"
          strokeDasharray={c}
          strokeDashoffset={c * (1 - (f ?? 0.3))}
          className="transition-[stroke-dashoffset] duration-300"
        />
      </svg>
      <span className="text-[0.68rem] font-semibold text-ink">{running.length > 0 ? running.length : failed ? "!" : <Check className="h-3.5 w-3.5" />}</span>
    </button>
  );
}

/**
 * The operations under way, at the foot of the sidebar: one card each
 * (newest last, next to the edge), or a ring when the sidebar is collapsed.
 * Absent while there are none.
 */
export function TaskTray({
  tasks,
  collapsed,
  onCancel,
  onDismiss,
  onDetails,
}: {
  tasks: OpTask[];
  collapsed: boolean;
  onCancel: (id: number) => void;
  onDismiss: (id: number) => void;
  onDetails: (ids: number[]) => void;
}) {
  if (tasks.length === 0) return null;
  if (collapsed) return <Ring tasks={tasks} onClick={() => onDetails(tasks.map((t) => t.id))} />;
  return (
    <div data-task-tray className="scroll -mr-1.5 flex max-h-[42vh] flex-col gap-2 overflow-y-auto pr-1.5">
      {tasks.map((t) => (
        <TaskCard key={t.id} task={t} onCancel={() => onCancel(t.id)} onDismiss={() => onDismiss(t.id)} onDetails={() => onDetails([t.id])} />
      ))}
    </div>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt className="text-dim">{label}</dt>
      <dd className="selectable min-w-0 break-words text-ink/90">{children}</dd>
    </>
  );
}

const STATE_TEXT: Record<OpTask["state"], Key> = { running: "tasks.underWay", done: "tasks.done", failed: "tasks.failed", cancelled: "tasks.cancelled" };

/** Everything known of one operation. */
function Details({ task, home }: { task: OpTask; home: string }) {
  const p = task.progress;
  const Icon = KIND_ICON[task.kind];
  const f = fraction(task);
  const byBytes = (p?.bytes_total ?? 0) > 0;
  const speed = task.state === "running" ? rate(task.samples, byBytes) : null;
  const left = secondsLeft(task);
  const elapsed = ((task.ended ?? Date.now()) - task.started) / 1000;
  const shown = task.sources.slice(0, 4);
  const state = task.cancelling && task.state === "running" ? t("tasks.cancellingLabel") : t(STATE_TEXT[task.state]);
  return (
    <section data-task-details={task.id}>
      <Heading
        icon={<Icon className={`h-4.5 w-4.5 ${toneOf(task) ? "" : "accent-fg"}`} style={toneOf(task) ? { color: toneOf(task) } : undefined} />}
        title={task.title}
        subtitle={f != null && task.state === "running" ? `${state} · ${Math.round(f * 100)}%` : state}
      />
      <Bar task={task} className="mt-4" />
      <dl className="mt-4 grid grid-cols-[5.5rem_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-[0.74rem]">
        {task.sources.length > 0 && (
          <Row label={task.kind === "trash" || task.kind === "delete" ? t("tasks.row.items") : t("tasks.row.from")}>
            {shown.map((s) => (
              <span key={s} className="block">
                {tildePath(s, home)}
              </span>
            ))}
            {task.sources.length > shown.length && <span className="block text-dim">{t("tasks.andMore", { count: task.sources.length - shown.length })}</span>}
          </Row>
        )}
        {task.dest && <Row label={task.kind === "compress" || task.kind === "extract" ? t("tasks.row.in") : t("tasks.row.to")}>{tildePath(task.dest, home)}</Row>}
        {task.state === "running" && p?.current && <Row label={t("tasks.row.now")}>{tildePath(p.current, home)}</Row>}
        {p && p.bytes_total > 0 && (
          <Row label={t("tasks.row.size")}>{t("tasks.bytesOf", { done: bytesText(p.bytes_done), total: bytesText(p.bytes_total) })}</Row>
        )}
        {p && (p.files_total > 0 || p.files_done > 0) && (
          <Row label={t("tasks.row.files")}>{p.files_total > 0 ? t("tasks.countOf", { done: p.files_done, total: p.files_total }) : num(p.files_done)}</Row>
        )}
        {p && p.items_total > 1 && (
          <Row label={t("tasks.row.items")}>{t("tasks.countOf", { done: p.items_done, total: p.items_total })}</Row>
        )}
        {speed != null && (
          <Row label={t("tasks.row.speed")}>{byBytes ? t("tasks.bytesPerSecond", { size: bytesText(speed) }) : t("tasks.filesPerSecond", { count: Math.round(speed) })}</Row>
        )}
        {left != null && <Row label={t("tasks.row.timeLeft")}>{left < 5 ? t("tasks.fewSeconds") : duration(left)}</Row>}
        <Row label={t("tasks.row.elapsed")}>{elapsed < 1 ? t("tasks.underASecond") : duration(elapsed)}</Row>
      </dl>
      {task.state === "failed" && task.error && (
        <p className="selectable mt-4 rounded-xl bg-[rgb(255_92_122/0.12)] px-3 py-2 text-[0.74rem] text-coral ring-1 ring-[rgb(255_92_122/0.3)]">{task.error}</p>
      )}
      {task.state === "cancelled" && (
        <p className="mt-4 text-[0.72rem] text-muted">{t("tasks.stoppedNote")}</p>
      )}
    </section>
  );
}

/** The eye's dialog: the details of one operation (or of all, from the ring), live. */
export function TaskDetails({
  tasks,
  home,
  onCancel,
  onClose,
}: {
  tasks: OpTask[];
  home: string;
  onCancel: (id: number) => void;
  onClose: () => void;
}) {
  const running = tasks.filter((t) => t.state === "running" && !t.cancelling);
  return (
    <Modal onCancel={onClose} className="w-[34rem]">
      <div className="scroll -mr-2 max-h-[65vh] space-y-6 overflow-y-auto pr-2">
        {tasks.length === 0 ? <p className="text-[0.8rem] text-muted">{t("tasks.none")}</p> : tasks.map((t) => <Details key={t.id} task={t} home={home} />)}
      </div>
      <div className="mt-5 flex justify-end gap-2">
        {running.length > 0 && (
          <button type="button" className="btn btn-danger" onClick={() => running.forEach((t) => onCancel(t.id))}>
            {t("tasks.cancelOperations", { count: running.length })}
          </button>
        )}
        <button type="button" className="btn btn-ghost" onClick={onClose} autoFocus>
          {t("common.close")}
        </button>
      </div>
    </Modal>
  );
}
