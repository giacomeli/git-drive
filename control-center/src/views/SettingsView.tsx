import { Eye, EyeOff, FileCog, KeyRound, LoaderCircle, RotateCw, ShieldAlert } from "lucide-react";
import { useState, type ReactNode } from "react";
import { errorText, type Config } from "../lib/api";
import { tildify } from "../lib/format";
import { useConfig } from "../lib/useConfig";
import { PageHeader } from "../components/Shell";
import { Card, Toggle, useToast } from "../components/ui";
import { RecordingFields } from "./RecordingPanel";
import { ThemeCard } from "./ThemeCard";

type Errors = Partial<Record<keyof Config, string>>;

function validate(c: Config): Errors {
  const e: Errors = {};
  const nonNegative: (keyof Config)[] = [
    "write_debounce_secs",
    "pull_interval_secs",
    "read_cache_mb",
    "large_file_threshold_mb",
    "chunk_threshold_mb",
    "commit_batch_mb",
    "log_max_mb",
    "log_keep_files",
    "p2p_idle_pull_interval_secs",
  ];
  for (const key of nonNegative) {
    const v = c[key] as number;
    if (!Number.isInteger(v) || v < 0) e[key] = "Whole number, 0 or more";
  }
  if (!Number.isInteger(c.chunk_size_mb) || c.chunk_size_mb < 1) e.chunk_size_mb = "At least 1 MiB";
  if (c.write_debounce_secs < 1) e.write_debounce_secs = "At least 1 second";
  if (c.user_email && !/^[^\s@]+@[^\s@]+$/.test(c.user_email)) e.user_email = "Not an email address";
  if (Boolean(c.user_name?.trim()) !== Boolean(c.user_email?.trim())) {
    e.user_name = "Name and email go together";
  }
  if (c.p2p_relay_url && !/^https?:\/\//.test(c.p2p_relay_url)) e.p2p_relay_url = "Must start with http:// or https://";
  if (c.repo_quota_gb != null && (!Number.isInteger(c.repo_quota_gb) || c.repo_quota_gb < 0)) e.repo_quota_gb = "Whole number of GB, or empty";
  return e;
}

export function SettingsView({ daemonRunning }: { daemonRunning: boolean }) {
  const toast = useToast();
  const cfg = useConfig();
  const c = cfg.draft;
  const errors = c ? validate(c) : {};
  const valid = Object.keys(errors).length === 0;

  const save = async (restart: boolean) => {
    try {
      await cfg.save(restart);
      toast("ok", restart ? "Settings saved. The daemon is restarting with them." : "Settings saved. They apply when the daemon restarts.");
    } catch (e) {
      toast("error", errorText(e));
    }
  };

  if (!c) {
    return (
      <div className="fade-in">
        <PageHeader title="Settings" />
        <p className="text-sm text-dim">{cfg.error ?? "Loading"}</p>
      </div>
    );
  }

  const num = (key: keyof Config) => ({
    value: c[key] as number,
    onChange: (v: number) => cfg.update(key, v as never),
    error: errors[key],
  });
  const text = (key: keyof Config) => ({
    value: (c[key] as string | null | undefined) ?? "",
    onChange: (v: string) => cfg.update(key, v as never),
    error: errors[key],
  });

  return (
    <div className="fade-in pb-24">
      <PageHeader
        title="Settings"
        subtitle={
          <span className="inline-flex items-center gap-1.5">
            <FileCog className="h-3.5 w-3.5" />
            {tildify(cfg.view?.path ?? "")}
            {!cfg.view?.exists && <span className="text-amber">· not created yet, defaults shown</span>}
          </span>
        }
      />

      <div className="mb-5">
        <ThemeCard />
      </div>

      <div className="grid grid-cols-1 items-start gap-5 xl:grid-cols-2">
        <div className="space-y-5">
          <Card title="Sync" subtitle="When local writes are committed and how often the remote is checked">
            <Row label="Commit after idle" hint="Seconds without writes before a commit">
              <NumberField {...num("write_debounce_secs")} unit="s" />
            </Row>
            <Row label="Pull interval" hint="Check the remote every N seconds; 0 turns the periodic pull off">
              <NumberField {...num("pull_interval_secs")} unit="s" />
            </Row>
            <Row label="Batch size" hint="Content per commit; larger drains split into several commits. 0 = one commit">
              <NumberField {...num("commit_batch_mb")} unit="MiB" />
            </Row>
          </Card>

          <Card title="Identity and remote" subtitle="Who signs the commits and how the remote is reached">
            <Row label="Name" hint="Without a name and email, writes are never committed">
              <TextField {...text("user_name")} placeholder="Your name" />
            </Row>
            <Row label="Email">
              <TextField {...text("user_email")} placeholder="you@example.com" />
            </Row>
            <Row label="Access token" hint="HTTPS token (GitHub PAT). Empty = SSH agent or credential helper">
              <SecretField {...text("remote_token")} />
            </Row>
            <Row label="Repository quota" hint="How much the repository may hold, shown on the Overview. Empty = the host's (GitHub, GitLab.com: 10 GB)">
              <NumberField
                value={c.repo_quota_gb ?? NaN}
                onChange={(v) => cfg.update("repo_quota_gb", Number.isNaN(v) ? null : v)}
                unit="GB"
                error={errors.repo_quota_gb}
              />
            </Row>
            <Row label="Transport" hint="auto uses the system git when it is installed">
              <select className="field !w-44" value={c.git_transport} onChange={(e) => cfg.update("git_transport", e.target.value)}>
                <option value="auto">auto</option>
                <option value="cli">cli (system git)</option>
                <option value="libgit2">libgit2</option>
              </select>
            </Row>
          </Card>

          <Card title="Storage and large files" subtitle="Memory use and how big files are stored in the repository">
            <Row label="Read cache" hint="Memory for small files; 0 turns it off">
              <NumberField {...num("read_cache_mb")} unit="MiB" />
            </Row>
            <Row label="Spool to disk above" hint="Write buffers above this go to disk instead of RAM; 0 = always RAM">
              <NumberField {...num("large_file_threshold_mb")} unit="MiB" />
            </Row>
            <Row label="Split files above" hint="Stored as chunks below GitHub's 100 MB file limit; 0 = never split">
              <NumberField {...num("chunk_threshold_mb")} unit="MiB" />
            </Row>
            <Row label="Chunk size">
              <NumberField {...num("chunk_size_mb")} unit="MiB" />
            </Row>
          </Card>
        </div>

        <div className="space-y-5">
          <Card title="Encryption" subtitle="Content and names encrypted in the repository; the drive shows plain files">
            <Row label="Encrypt the repository" hint="Uses the drive key; losing the key means losing the data">
              <Toggle checked={c.encryption_enabled} onChange={(v) => cfg.update("encryption_enabled", v)} />
            </Row>
            {c.encryption_enabled !== cfg.view?.config.encryption_enabled && (
              <Warning>
                Changing this rewrites every file of the drive on the next sync. History from before the change stays as it was.
              </Warning>
            )}
            <Row label="Key file" hint="Empty = the default key next to the config">
              <TextField {...text("encryption_key_file")} placeholder="~/.config/git-drive/drive.key" mono />
            </Row>
          </Card>

          <Card title="P2P channel" subtitle="Devices announce pushes to each other, so changes arrive in seconds">
            <Row label="Enable the channel" hint="Off = the drive syncs by polling the remote only">
              <Toggle checked={c.p2p_enabled} onChange={(v) => cfg.update("p2p_enabled", v)} />
            </Row>
            <Row label="Device name" hint="How this machine appears to the others; empty = host name">
              <TextField {...text("p2p_device_name")} placeholder="host name" />
            </Row>
            <Row label="Pull interval while online" hint="Safety-net polling when announcements arrive over the channel">
              <NumberField {...num("p2p_idle_pull_interval_secs")} unit="s" />
            </Row>
            <Row label="Relay server" hint="Empty = the public relays">
              <TextField {...text("p2p_relay_url")} placeholder="https://relay.example.com" mono />
            </Row>
            <Row label="Device key file" hint="Empty = peer.key next to the config">
              <TextField {...text("p2p_key_file")} placeholder="~/.config/git-drive/peer.key" mono />
            </Row>
          </Card>

          <Card title="Logs" subtitle="What the daemon records and how the files rotate">
            <Row label="Rotate at" hint="Size of one log file; 0 = never rotate by size">
              <NumberField {...num("log_max_mb")} unit="MiB" />
            </Row>
            <Row label="Keep" hint="Rotated files kept before the oldest are deleted">
              <NumberField {...num("log_keep_files")} unit="files" />
            </Row>
            <div className="border-t border-line pt-4">
              <RecordingFields config={c} update={cfg.update} />
            </div>
          </Card>
        </div>

      </div>

      {cfg.dirty && (
        <div className="fixed bottom-6 left-1/2 z-40 -translate-x-1/2 pl-60">
          <div className="glass glass-strong fade-in flex items-center gap-4 py-2.5 pl-5 pr-2.5" style={{ borderRadius: 9999 }}>
            <p className="text-[0.8rem] text-muted">{valid ? "Unsaved changes" : "Fix the highlighted fields"}</p>
            <button className="btn btn-ghost" disabled={cfg.saving} onClick={cfg.discard}>
              Discard
            </button>
            <button className="btn btn-ghost" disabled={cfg.saving || !valid} onClick={() => save(false)}>
              Save
            </button>
            {daemonRunning && (
              <button className="btn btn-primary" disabled={cfg.saving || !valid} onClick={() => save(true)}>
                {cfg.saving ? <LoaderCircle className="spin h-3.5 w-3.5" /> : <RotateCw className="h-3.5 w-3.5" />}
                Save &amp; restart
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex items-center gap-6 border-t border-line py-3.5 first:border-t-0 first:pt-0 last:pb-0">
      <div className="min-w-0 flex-1">
        <p className="text-[0.84rem] font-medium">{label}</p>
        {hint && <p className="mt-0.5 text-[0.7rem] text-dim">{hint}</p>}
      </div>
      <div className="flex flex-none justify-end">{children}</div>
    </div>
  );
}

function FieldError({ error }: { error?: string }) {
  return error ? <p className="mt-1 text-right text-[0.68rem] text-coral">{error}</p> : null;
}

function NumberField({ value, onChange, unit, error }: { value: number; onChange: (v: number) => void; unit: string; error?: string }) {
  return (
    <div className="w-36">
      <div className="relative">
        <input
          type="number"
          min={0}
          className={`field pr-14 tabular-nums ${error ? "invalid" : ""}`}
          value={Number.isFinite(value) ? value : ""}
          onChange={(e) => onChange(e.target.value === "" ? NaN : Number(e.target.value))}
        />
        <span className="pointer-events-none absolute right-3.5 top-1/2 -translate-y-1/2 text-[0.72rem] text-dim">{unit}</span>
      </div>
      <FieldError error={error} />
    </div>
  );
}

function TextField({
  value,
  onChange,
  placeholder,
  error,
  mono = false,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  error?: string;
  mono?: boolean;
}) {
  return (
    <div className="w-52">
      <input
        className={`field ${mono ? "font-mono !text-[0.75rem]" : ""} ${error ? "invalid" : ""}`}
        value={value}
        placeholder={placeholder}
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
      />
      <FieldError error={error} />
    </div>
  );
}

function SecretField({ value, onChange, error }: { value: string; onChange: (v: string) => void; error?: string }) {
  const [shown, setShown] = useState(false);
  return (
    <div className="w-52">
      <div className="relative">
        <KeyRound className="pointer-events-none absolute left-3.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-dim" />
        <input
          type={shown ? "text" : "password"}
          className={`field !pl-9 pr-10 font-mono !text-[0.75rem] ${error ? "invalid" : ""}`}
          value={value}
          placeholder="not set"
          spellCheck={false}
          autoComplete="off"
          onChange={(e) => onChange(e.target.value)}
        />
        <button
          type="button"
          onClick={() => setShown((s) => !s)}
          className="absolute right-3 top-1/2 -translate-y-1/2 text-dim hover:text-ink"
          aria-label={shown ? "Hide token" : "Show token"}
        >
          {shown ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
        </button>
      </div>
      <FieldError error={error} />
    </div>
  );
}

function Warning({ children }: { children: ReactNode }) {
  return (
    <div className="my-1 flex items-start gap-2.5 rounded-2xl border border-amber/30 bg-amber/10 px-4 py-3 text-[0.75rem] text-[#ffd79a]">
      <ShieldAlert className="mt-0.5 h-4 w-4 flex-none" />
      <p>{children}</p>
    </div>
  );
}
