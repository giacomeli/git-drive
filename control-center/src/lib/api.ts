import { invoke } from "@tauri-apps/api/core";

// Mirrors of the Rust types the commands return (src-tauri/src/main.rs and
// the git_drive library). Field names stay snake_case, as serde writes them.

export type ServiceState = "running" | "stopped" | "not_installed";

export interface PeerInfo {
  name: string;
  id: string;
  online: boolean;
  last_seen: number | null;
}

export interface P2pInfo {
  online: boolean;
  device_id: string;
  device_name: string;
  peers: PeerInfo[];
}

export interface DrainInfo {
  at: number;
  ops: number;
  commit: string | null;
  pushed: boolean | null;
}

export interface PullInfo {
  at: number;
  outcome: string;
}

export interface SyncingInfo {
  since: number;
  ops: number;
  /** "committing", then "publishing". */
  stage: string;
}

export interface SyncStatus {
  pid: number;
  repo: string;
  branch: string;
  mount: string;
  log_file: string;
  started_at: number;
  updated_at: number;
  stopped_at: number | null;
  /** null while the session boots; absent in status files of older daemons. */
  mounted_at?: number | null;
  pending_ops: number;
  /** A drain at work (its changes left the queue); absent in status files of older daemons. */
  syncing?: SyncingInfo | null;
  unpushed: number;
  last_drain: DrainInfo | null;
  last_pull: PullInfo | null;
  conflicts: string[];
  commit_blocked: string | null;
  p2p: P2pInfo | null;
  /** The repository against what its host accepts; absent in status files of older daemons. */
  repo_usage?: RepoUsage | null;
}

/** The repository's object store against its host's limits (the daemon measures it at boot and after each commit and pull). */
export interface RepoUsage {
  used: number;
  measured_at: number;
  /** "GitHub", "GitLab.com", or the remote's host name. */
  host: string;
  /** The host's recommended maximum, or `repo_quota_gb`; null when unknown. */
  limit: number | null;
  /** A lower size the host strongly recommends staying under. */
  recommended: number | null;
  from_config: boolean;
}

export interface Volume {
  total: number;
  free: number;
}

export interface Overview {
  now: number;
  service: ServiceState;
  status: SyncStatus | null;
  volume: Volume | null;
  repo_bytes: number | null;
  activity: number[];
}

export interface LogEntry {
  time: string;
  level: string;
  module: string;
  category: string;
  message: string;
}

export interface LogTail {
  file: string;
  entries: LogEntry[];
}

export interface Config {
  write_debounce_secs: number;
  pull_interval_secs: number;
  read_cache_mb: number;
  large_file_threshold_mb: number;
  chunk_threshold_mb: number;
  chunk_size_mb: number;
  commit_batch_mb: number;
  encryption_enabled: boolean;
  encryption_key_file?: string | null;
  user_name?: string | null;
  user_email?: string | null;
  remote_token?: string | null;
  log_max_mb: number;
  log_keep_files: number;
  log_level: string;
  log_categories?: Record<string, string>;
  git_transport: string;
  p2p_enabled: boolean;
  p2p_idle_pull_interval_secs: number;
  p2p_key_file?: string | null;
  p2p_device_name?: string | null;
  p2p_relay_url?: string | null;
  /** How much the repository may hold, in GB; null = the host's limit. */
  repo_quota_gb?: number | null;
}

export interface ConfigView {
  path: string;
  exists: boolean;
  config: Config;
}

export interface PathInfo {
  label: string;
  path: string;
  exists: boolean;
}

export type Glass = "liquid-glass" | "vibrancy" | "none";

export interface About {
  version: string;
  platform: "macOS" | "Linux";
  glass: Glass;
  service_installed: boolean;
  service_name: string;
  git_transport: string;
  encryption: boolean;
  p2p: boolean;
  paths: PathInfo[];
}

export type DaemonAction = "start" | "stop" | "restart";

/** Preferences of this window (`~/.config/git-drive/control-center.json`), not of the daemon. */
export interface UiPrefs {
  accent: string;
}

/** Levels a log category (or the base level) can take. */
export const LEVELS = ["off", "error", "warn", "info", "debug", "trace"] as const;

/** Log categories, in display order, with what each one covers. */
export const CATEGORIES: { id: string; label: string; hint: string }[] = [
  { id: "sync", label: "Sync", hint: "Drain, commit, push, pull and the journal" },
  { id: "git", label: "Git", hint: "Transport, fetch and merge" },
  { id: "fuse", label: "Filesystem", hint: "The mounted drive and range reads" },
  { id: "crypto", label: "Encryption", hint: "Encryption and chunking" },
  { id: "p2p", label: "P2P", hint: "The device channel" },
  { id: "network", label: "Network", hint: "The P2P network stack (iroh), very chatty" },
  { id: "service", label: "Service", hint: "Boot, shutdown, status and notifications" },
];

export const api = {
  overview: () => invoke<Overview>("overview"),
  logs: (max: number) => invoke<LogTail>("logs", { max }),
  getConfig: () => invoke<ConfigView>("get_config"),
  saveConfig: (config: Config) => invoke<void>("save_config", { config }),
  daemon: (action: DaemonAction) => invoke<void>("daemon", { action }),
  openPath: (path: string) => invoke<void>("open_path", { path }),
  about: () => invoke<About>("about"),
  uiPrefs: () => invoke<UiPrefs>("get_ui_prefs"),
  saveUiPrefs: (prefs: UiPrefs) => invoke<void>("save_ui_prefs", { prefs }),
};

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
