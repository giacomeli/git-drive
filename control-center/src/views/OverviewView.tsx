import {
  ArrowDownToLine,
  CircleAlert,
  Clock,
  Copy,
  FolderOpen,
  GitBranch,
  GitCommitHorizontal,
  Laptop,
  Radio,
  RefreshCcw,
  Server,
  Split,
  UploadCloud,
} from "lucide-react";
import type { ReactNode } from "react";
import { api, errorText, type Overview, type PeerInfo, type RepoUsage } from "../lib/api";
import { ago, bytes, bytesText, duration, shortId, tildify } from "../lib/format";
import { health } from "../lib/health";
import { AreaChart, DiskRing, HeroWaves, MeterBar } from "../components/Charts";
import { PageHeader } from "../components/Shell";
import { Card, Pill, StatusDot, toneColor, useToast } from "../components/ui";

export function OverviewView({ overview }: { overview: Overview | null }) {
  const toast = useToast();
  const s = overview?.status ?? null;
  const running = overview?.service === "running";
  const openDrive = () => s && api.openPath(s.mount).catch((e) => toast("error", errorText(e)));

  return (
    <div className="fade-in">
      <PageHeader
        title="Overview"
        subtitle={
          s ? (
            <span className="inline-flex items-center gap-2">
              <span>{tildify(s.mount)}</span>
              <span className="text-dim">·</span>
              <GitBranch className="h-3.5 w-3.5 text-dim" />
              <span>{s.branch}</span>
            </span>
          ) : (
            "Status of the drive on this machine"
          )
        }
        actions={
          running && (
            <button className="btn btn-ghost" onClick={openDrive}>
              <FolderOpen className="h-4 w-4" />
              Open drive
            </button>
          )
        }
      />

      {overview?.service === "not_installed" && <InstallHint />}
      {s?.commit_blocked && running && (
        <div className="glass mb-5 flex items-start gap-3 border-coral/40 px-5 py-4" style={{ background: "rgb(255 92 122 / 0.12)" }}>
          <CircleAlert className="mt-0.5 h-5 w-5 flex-none text-coral" />
          <div className="min-w-0">
            <p className="text-sm font-semibold text-ink">Commits are blocked</p>
            <p className="selectable mt-0.5 text-xs text-muted">{s.commit_blocked}. The writes are kept in the working tree and the journal.</p>
          </div>
        </div>
      )}

      <div className="grid grid-cols-12 gap-4">
        <HeroCard overview={overview} onOpen={openDrive} />
        <StorageCard overview={overview} />
        <ActivityCard overview={overview} />
        <DevicesCard overview={overview} />
        <MetricTiles overview={overview} />
      </div>
    </div>
  );
}

function HeroCard({ overview, onOpen }: { overview: Overview | null; onOpen: () => void }) {
  const h = health(overview);
  const s = overview?.status;
  const now = overview?.now ?? 0;
  const drain = s?.last_drain;
  const running = overview?.service === "running" && s?.mounted_at !== null;
  return (
    <section className="hero relative col-span-12 min-h-[13.5rem] overflow-hidden p-6 text-white lg:col-span-7">
      <HeroWaves />
      <div className="relative">
        <div className="flex items-center gap-2">
          <span className="label !text-white/70">Sync state</span>
        </div>
        <p className="mt-3 text-[2.5rem] font-semibold leading-none tracking-tight">{h.title}</p>
        <p className="mt-3 max-w-md text-[0.82rem] text-white/80">{h.detail}</p>
        {drain && (
          <p className="mt-5 inline-flex items-center gap-2 rounded-full bg-white/15 px-3 py-1.5 text-[0.72rem] font-medium backdrop-blur">
            <GitCommitHorizontal className="h-3.5 w-3.5" />
            Last commit {ago(drain.at, now)}
            {drain.commit && <span className="font-mono text-white/75">{drain.commit.slice(0, 7)}</span>}
            {drain.pushed === true && <span className="text-white/75">· published</span>}
            {drain.pushed === false && <span className="text-[#ffe0e6]">· push failed</span>}
          </p>
        )}
      </div>
      {running && (
        <button
          onClick={onOpen}
          title="Open drive"
          aria-label="Open drive"
          style={{ color: "var(--a2)" }}
          className="absolute bottom-6 right-6 grid h-14 w-14 place-items-center rounded-full bg-white shadow-xl shadow-black/25 transition-transform hover:scale-105 active:scale-95"
        >
          <FolderOpen className="h-5 w-5" strokeWidth={2.2} />
        </button>
      )}
    </section>
  );
}

function StorageCard({ overview }: { overview: Overview | null }) {
  const v = overview?.volume;
  const repo = overview?.repo_bytes ?? 0;
  const free = bytes(v?.free);
  const usedOther = v ? Math.max(v.total - v.free - repo, 0) : 0;
  return (
    <Card
      className="col-span-12 lg:col-span-5"
      title="Storage"
      subtitle="Volume that holds the repository"
      action={v && <span className="text-[0.72rem] font-medium text-muted">{Math.round((1 - v.free / v.total) * 100)}% used</span>}
    >
      {v ? (
        <div className="flex items-center gap-5">
          <div className="relative flex-none">
            <DiskRing total={v.total} free={v.free} repo={repo} size={156} />
            <div className="absolute inset-0 grid place-items-center text-center">
              <div>
                <p className="text-[1.7rem] font-semibold leading-none tracking-tight">
                  {free.value}
                  <span className="ml-1 text-sm font-medium text-muted">{free.unit}</span>
                </p>
                <p className="mt-1.5 text-[0.7rem] text-dim">free of {bytesText(v.total)}</p>
              </div>
            </div>
          </div>
          <ul className="min-w-0 flex-1 space-y-3">
            <Legend color="linear-gradient(135deg, var(--a3), var(--a1))" label="Repository" value={overview?.repo_bytes == null ? "measuring" : bytesText(repo)} />
            <Legend color="linear-gradient(135deg, var(--s1), var(--s2))" label="Other data" value={bytesText(usedOther)} />
            <Legend color="rgb(255 255 255 / 0.18)" label="Available" value={bytesText(v.free)} />
          </ul>
        </div>
      ) : (
        <Empty>Start the daemon to read the repository volume.</Empty>
      )}
      {v && overview?.status?.repo_usage && <RepoCapacity usage={overview.status.repo_usage} />}
    </Card>
  );
}

/**
 * The repository against what its host accepts: GitHub publishes no quota,
 * so its recommended maximum (10 GB) is the bar's end and the size it
 * strongly recommends staying under (5 GB) a mark on it; `repo_quota_gb`
 * replaces the end. Amber past the mark or 80%, coral past the end.
 */
function RepoCapacity({ usage: u }: { usage: RepoUsage }) {
  const where = u.host ? ` on ${u.host}` : "";
  const over = u.limit != null && u.used >= u.limit;
  const warn = !over && ((u.recommended != null && u.used >= u.recommended) || (u.limit != null && u.used > 0.8 * u.limit));
  const [from, to] = over
    ? ["var(--color-coral)", "var(--color-coral)"]
    : warn
      ? ["var(--color-amber)", "var(--color-amber)"]
      : ["var(--a3)", "var(--a1)"];
  const note =
    u.limit == null
      ? `${u.host || "This host"} publishes no size limit; set repo_quota_gb in the config to show one.`
      : u.from_config
        ? `Your quota (repo_quota_gb).${u.recommended != null ? ` ${u.host} strongly recommends staying under ${bytesText(u.recommended)} (the mark).` : ""}`
        : over
          ? u.host === "GitHub"
            ? "Past GitHub's recommended maximum. Pushes still go through; GitHub refuses only a push over 2 GB or a file over 100 MB."
            : `Past ${u.host}'s limit: pushes may be refused.`
          : u.recommended != null
            ? `${u.host} recommends at most ${bytesText(u.limit)} and strongly recommends staying under ${bytesText(u.recommended)} (the mark).`
            : `${u.host} accepts up to ${bytesText(u.limit)} per repository.`;
  return (
    <div className="mt-5 border-t border-line pt-4">
      <div className="flex items-baseline justify-between gap-3">
        <p className="text-[0.8rem] font-semibold text-ink">Repository{where}</p>
        <p className="whitespace-nowrap text-[0.74rem] tabular-nums text-muted">
          {bytesText(u.used)}
          {u.limit != null && ` of ${u.from_config ? "your " : ""}${bytesText(u.limit)}`}
        </p>
      </div>
      {u.limit != null && (
        <div className="relative mt-2">
          <MeterBar fraction={u.used / u.limit} from={from} to={to} />
          {u.recommended != null && u.recommended < u.limit && (
            <span
              className="absolute -top-0.5 h-2.5 w-0.5 rounded-full bg-white/70"
              style={{ left: `calc(${(u.recommended / u.limit) * 100}% - 1px)` }}
            />
          )}
        </div>
      )}
      <p className="mt-2 text-[0.68rem] leading-relaxed text-dim">{note}</p>
    </div>
  );
}

function Legend({ color, label, value }: { color: string; label: string; value: string }) {
  return (
    <li className="flex items-start gap-3">
      <span className="mt-1.5 h-2.5 w-2.5 flex-none rounded-full" style={{ background: color }} />
      <div className="min-w-0">
        <p className="whitespace-nowrap text-[0.9rem] font-semibold leading-tight tabular-nums text-ink">{value}</p>
        <p className="mt-0.5 text-[0.7rem] text-dim">{label}</p>
      </div>
    </li>
  );
}

function ActivityCard({ overview }: { overview: Overview | null }) {
  const values = overview?.activity ?? [];
  const total = values.reduce((a, b) => a + b, 0);
  const lastHour = values[values.length - 1] ?? 0;
  return (
    <Card
      className="col-span-12 lg:col-span-7"
      title="Activity"
      subtitle="Commits per hour, last 24 hours"
      action={
        <div className="text-right">
          <p className="text-[1.35rem] font-semibold leading-none tracking-tight">{total}</p>
          <p className="mt-1 text-[0.68rem] text-dim">{lastHour} in the last hour</p>
        </div>
      }
    >
      <div className="relative h-32">
        {values.length > 0 ? (
          <>
            <AreaChart values={values} height={128} />
            {total === 0 && <p className="absolute inset-0 grid place-items-center text-xs text-dim">No commits in the last 24 hours</p>}
          </>
        ) : (
          <Empty>No history to show yet.</Empty>
        )}
      </div>
      <div className="mt-2 flex justify-between text-[0.66rem] text-dim">
        <span>24 h ago</span>
        <span>12 h ago</span>
        <span>now</span>
      </div>
    </Card>
  );
}

function DevicesCard({ overview }: { overview: Overview | null }) {
  const p2p = overview?.status?.p2p;
  const now = overview?.now ?? 0;
  const running = overview?.service === "running";
  const online = p2p?.peers.filter((p) => p.online).length ?? 0;
  return (
    <Card
      className="col-span-12 lg:col-span-5"
      title="Devices"
      subtitle={p2p ? `${online} of ${p2p.peers.length} connected` : "P2P notification channel"}
      action={
        p2p ? (
          <Pill tone={p2p.online ? "mint" : "amber"}>{p2p.online ? "Channel online" : "Channel offline"}</Pill>
        ) : (
          <Pill tone="dim">{running ? "P2P off" : "Daemon stopped"}</Pill>
        )
      }
    >
      {p2p ? (
        <ul className="-mx-1 space-y-1">
          <DeviceRow name={p2p.device_name} id={p2p.device_id} self online={p2p.online} />
          {p2p.peers.map((peer) => (
            <PeerRow key={peer.id} peer={peer} now={now} />
          ))}
          {p2p.peers.length === 0 && <p className="px-1 pt-2 text-xs text-dim">No other device registered in this drive yet.</p>}
        </ul>
      ) : (
        <Empty>
          {running
            ? "Turn on the P2P channel in Settings so devices announce pushes to each other."
            : "Start the daemon to see the devices of this drive."}
        </Empty>
      )}
    </Card>
  );
}

function PeerRow({ peer, now }: { peer: PeerInfo; now: number }) {
  return (
    <DeviceRow
      name={peer.name || "Unnamed device"}
      id={peer.id}
      online={peer.online}
      seen={peer.online ? "Connected" : peer.last_seen ? `Seen ${ago(peer.last_seen, now)}` : "Never seen"}
    />
  );
}

function DeviceRow({ name, id, online, self = false, seen }: { name: string; id: string; online: boolean; self?: boolean; seen?: string }) {
  const Icon = self ? Laptop : Server;
  return (
    <li className="flex items-center gap-3.5 rounded-2xl px-2 py-2.5 transition-colors hover:bg-white/5">
      <div
        className="grid h-11 w-11 flex-none place-items-center rounded-full"
        style={{
          background: self ? "var(--accent)" : "rgb(255 255 255 / 0.06)",
          border: self ? "none" : "1px solid rgb(255 255 255 / 0.12)",
          boxShadow: self ? "0 8px 20px -8px rgb(var(--a3-rgb) / 0.8)" : undefined,
        }}
      >
        <Icon className={`h-5 w-5 ${self ? "text-white" : "text-muted"}`} />
      </div>
      <div className="min-w-0 flex-1">
        <p className="truncate text-[0.88rem] font-semibold">
          {name}
          {self && <span className="ml-2 text-[0.66rem] font-medium text-dim">this device</span>}
        </p>
        <p className="font-mono text-[0.68rem] text-dim">{shortId(id)}</p>
      </div>
      {!self && (
        <span className="flex items-center gap-2 text-[0.72rem]" style={{ color: online ? toneColor("mint") : toneColor("dim") }}>
          <StatusDot tone={online ? "mint" : "dim"} />
          {seen}
        </span>
      )}
    </li>
  );
}

function MetricTiles({ overview }: { overview: Overview | null }) {
  const s = overview?.status;
  const now = overview?.now ?? 0;
  const running = overview?.service === "running" && s;
  const uptime = running ? now - s.started_at : 0;
  const pull = s?.last_pull;
  return (
    <div className="col-span-12 grid grid-cols-2 gap-4 lg:grid-cols-4">
      <Metric
        icon={<Clock className="h-4 w-4" />}
        label="Uptime"
        value={running ? duration(uptime) : "-"}
        hint={running ? `since ${new Date(s.started_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}` : "daemon stopped"}
        fraction={Math.min(uptime / 86400, 1)}
        colors={["#46e3a4", "#3fd0f0"]}
      />
      <Metric
        icon={<ArrowDownToLine className="h-4 w-4" />}
        label="Last pull"
        value={pull ? ago(pull.at, now) : "-"}
        hint={pull?.outcome ?? "no pull yet"}
        fraction={pull ? Math.max(0, 1 - (now - pull.at) / 600) : 0}
        colors={["var(--s1)", "var(--a1)"]}
      />
      <Metric
        icon={<UploadCloud className="h-4 w-4" />}
        label="Waiting"
        value={running ? String((s.pending_ops ?? 0) + (s.unpushed ?? 0)) : "-"}
        hint={running ? `${s.pending_ops} to commit · ${s.unpushed} to publish` : "-"}
        fraction={running ? Math.min(((s.pending_ops ?? 0) + (s.unpushed ?? 0)) / 20, 1) : 0}
        colors={["#ffb547", "#ff5c7a"]}
      />
      <Metric
        icon={<Split className="h-4 w-4" />}
        label="Conflicts"
        value={s ? String(s.conflicts.length) : "-"}
        hint={s && s.conflicts.length > 0 ? "conflicted copies this session" : "none this session"}
        fraction={s ? Math.min(s.conflicts.length / 5, 1) : 0}
        colors={["#c86bf0", "#ff5c7a"]}
      />
    </div>
  );
}

function Metric({
  icon,
  label,
  value,
  hint,
  fraction,
  colors,
}: {
  icon: ReactNode;
  label: string;
  value: string;
  hint: string;
  fraction: number;
  colors: [string, string];
}) {
  return (
    <section className="glass px-5 py-4">
      <div className="flex items-center gap-2 text-muted">
        {icon}
        <span className="text-[0.75rem] font-medium">{label}</span>
      </div>
      <p className="mt-2.5 truncate text-[1.5rem] font-semibold leading-none tracking-tight">{value}</p>
      <p className="mb-3 mt-1.5 truncate text-[0.7rem] text-dim">{hint}</p>
      <MeterBar fraction={fraction} from={colors[0]} to={colors[1]} />
    </section>
  );
}

function InstallHint() {
  const toast = useToast();
  const command = "git-drive install -r <repository> -m <mount point> --with-tray";
  return (
    <div className="glass mb-5 flex items-center gap-4 px-5 py-4">
      <Radio className="h-5 w-5 flex-none text-amber" />
      <div className="min-w-0 flex-1">
        <p className="text-sm font-semibold">The git-drive service is not installed</p>
        <p className="selectable mt-1 font-mono text-xs text-muted">{command}</p>
      </div>
      <button
        className="btn btn-ghost"
        onClick={() => navigator.clipboard.writeText(command).then(() => toast("ok", "Command copied"))}
      >
        <Copy className="h-3.5 w-3.5" />
        Copy
      </button>
    </div>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return (
    <div className="grid h-full min-h-28 place-items-center rounded-2xl border border-dashed border-white/10 px-6 text-center text-xs text-dim">
      <span className="inline-flex items-center gap-2">
        <RefreshCcw className="h-3.5 w-3.5 flex-none" />
        {children}
      </span>
    </div>
  );
}
