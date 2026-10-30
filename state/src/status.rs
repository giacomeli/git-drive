//! Observable daemon state, written to `logs/status.json` and read by the
//! `git-drive status` subcommand. Timestamps are unix seconds (u64) — human
//! formatting happens only in the CLI (decision 10 of Phase 1).

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::RepoCapacity;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncStatus {
    pub pid: u32,
    pub repo: String,
    pub branch: String,
    /// Volume mount point (for "Open in Finder" in the tray).
    #[serde(default)]
    pub mount: String,
    /// Current session log (for the live tail in the tray).
    #[serde(default)]
    pub log_file: String,
    pub started_at: u64,
    pub updated_at: u64,
    /// Set on clean shutdown (SIGTERM/eject with final drain).
    #[serde(default)]
    pub stopped_at: Option<u64>,
    /// When the volume appeared. `None` while the session boots (journal
    /// replay, recovery drain, index): the daemon runs but the drive is not
    /// there yet.
    #[serde(default)]
    pub mounted_at: Option<u64>,
    pub pending_ops: usize,
    /// A drain at work on the changes it took from the queue (so they no
    /// longer count in `pending_ops`). `None` between drains.
    #[serde(default)]
    pub syncing: Option<SyncingInfo>,
    /// Local commits still awaiting publication to the remote (progressive
    /// push catch-up in flight or interrupted). 0 = remote at the local tip.
    #[serde(default)]
    pub unpushed: usize,
    pub last_drain: Option<DrainInfo>,
    pub last_pull: Option<PullInfo>,
    /// Conflicted copies created in this session (paths of the `*.conflict-*` copies).
    pub conflicts: Vec<String>,
    /// Why the last drain could not commit (the writes stay in the working
    /// tree and the journal). `None` once a drain commits again.
    #[serde(default)]
    pub commit_blocked: Option<String>,
    /// P2P notification channel; `None` = off (or a build without it).
    #[serde(default)]
    pub p2p: Option<P2pInfo>,
    /// The repository's size against what its host accepts. `None` until
    /// the boot measured it.
    #[serde(default)]
    pub repo_usage: Option<RepoUsage>,
}

/// The repository's size (`storage::objects_size`), measured at boot and
/// after every change of HEAD, against its host's limits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoUsage {
    pub used: u64,
    pub measured_at: u64,
    #[serde(flatten)]
    pub capacity: RepoCapacity,
}

/// A drain in progress.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct SyncingInfo {
    pub since: u64,
    pub ops: usize,
    /// `committing` while the changes reach the working tree and become
    /// commits, then `publishing` while they are pushed.
    pub stage: String,
}

/// State of the P2P channel as published by the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct P2pInfo {
    pub online: bool,
    pub device_id: String,
    pub device_name: String,
    pub peers: Vec<PeerInfo>,
}

/// A registered device other than this one.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PeerInfo {
    /// Empty when the registry entry could not be read.
    pub name: String,
    pub id: String,
    pub online: bool,
    pub last_seen: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainInfo {
    pub at: u64,
    pub ops: usize,
    pub commit: Option<String>,
    /// `None` = no remote configured or no commit; `Some(false)` = push
    /// failed even after the reconcile.
    pub pushed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullInfo {
    pub at: u64,
    pub outcome: String,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// In-memory state + optional file persistence. Always present in the
/// `SyncManager`; the file is only written after `enable_file`.
pub struct StatusState {
    inner: Mutex<SyncStatus>,
    file: Mutex<Option<PathBuf>>,
}

impl Default for StatusState {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusState {
    pub fn new() -> Self {
        StatusState {
            inner: Mutex::new(SyncStatus::default()),
            file: Mutex::new(None),
        }
    }

    pub fn record_boot(&self, repo: &str, branch: &str) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.pid = std::process::id();
        s.repo = repo.to_string();
        s.branch = branch.to_string();
        s.started_at = now;
        s.updated_at = now;
        // A new session invalidates the previous one's shutdown and mount (same status file).
        s.stopped_at = None;
        s.mounted_at = None;
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Turns on persistence and writes the current state immediately.
    pub fn enable_file(&self, path: PathBuf) {
        *self.file.lock() = Some(path);
        let snapshot = self.inner.lock().clone();
        self.persist(&snapshot);
    }

    /// Records the mount point and session log (consumed by the tray/status).
    pub fn set_runtime_info(&self, mount: &str, log_file: &str) {
        let mut s = self.inner.lock();
        s.mount = mount.to_string();
        s.log_file = log_file.to_string();
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// The volume is mounted: the boot finished.
    pub fn record_mounted(&self) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.updated_at = now;
        s.mounted_at = Some(now);
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Clean daemon shutdown (post-unmount + final drain).
    pub fn record_stopped(&self) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.updated_at = now;
        s.stopped_at = Some(now);
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    pub fn record_drain(
        &self,
        ops: usize,
        commit: Option<String>,
        pushed: Option<bool>,
        pending_ops: usize,
    ) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.updated_at = now;
        s.pending_ops = pending_ops;
        s.last_drain = Some(DrainInfo {
            at: now,
            ops,
            commit,
            pushed,
        });
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// A drain took `ops` changes from the queue and starts on them.
    pub fn start_syncing(&self, ops: usize) {
        let now = now_unix();
        self.update(|s| s.syncing = Some(SyncingInfo { since: now, ops, stage: "committing".into() }));
    }

    /// The drain moved on (`publishing` once its commits exist).
    pub fn set_sync_stage(&self, stage: &str) {
        self.update(|s| {
            if let Some(sy) = s.syncing.as_mut() {
                sy.stage = stage.to_string();
            }
        });
    }

    /// The drain ended, however it ended.
    pub fn end_syncing(&self) {
        self.update(|s| s.syncing = None);
    }

    fn update(&self, change: impl FnOnce(&mut SyncStatus)) {
        let mut s = self.inner.lock();
        s.updated_at = now_unix();
        change(&mut s);
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// The repository measured `used` bytes. `capacity` comes once, at boot;
    /// later measurements keep it (and before it, nothing is published).
    pub fn record_repo_size(&self, used: u64, capacity: Option<RepoCapacity>) {
        let mut s = self.inner.lock();
        let Some(capacity) = capacity.or_else(|| s.repo_usage.as_ref().map(|u| u.capacity.clone())) else {
            return;
        };
        s.repo_usage = Some(RepoUsage { used, measured_at: now_unix(), capacity });
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Whether the boot set the capacity, so a measurement would be published.
    pub fn measures_repo(&self) -> bool {
        self.inner.lock().repo_usage.is_some()
    }

    /// Commits still awaiting publication (progressive push). Doubles as a
    /// heartbeat: every call refreshes `updated_at`.
    pub fn set_unpushed(&self, n: usize) {
        let mut s = self.inner.lock();
        s.updated_at = now_unix();
        s.unpushed = n;
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Publishes the P2P channel state (`None` = channel off).
    pub fn set_p2p(&self, info: Option<P2pInfo>) {
        let mut s = self.inner.lock();
        s.updated_at = now_unix();
        s.p2p = info;
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Records (or clears, with `None`) a commit the drain could not make.
    pub fn set_commit_blocked(&self, reason: Option<String>) {
        let mut s = self.inner.lock();
        s.updated_at = now_unix();
        s.commit_blocked = reason;
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Pure heartbeat: refreshes `updated_at` so long operations (a multi-minute
    /// pack upload) don't read as a dead daemon in the tray.
    pub fn touch(&self) {
        let mut s = self.inner.lock();
        s.updated_at = now_unix();
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    pub fn record_pull(&self, outcome: &str) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.updated_at = now;
        s.last_pull = Some(PullInfo {
            at: now,
            outcome: outcome.to_string(),
        });
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Records conflicted copies created by merges (pull or reconcile).
    pub fn add_conflicts(&self, copies: Vec<String>) {
        if copies.is_empty() {
            return;
        }
        let mut s = self.inner.lock();
        s.updated_at = now_unix();
        s.conflicts.extend(copies);
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    pub fn snapshot(&self) -> SyncStatus {
        self.inner.lock().clone()
    }

    fn persist(&self, status: &SyncStatus) {
        let Some(path) = self.file.lock().clone() else {
            return;
        };
        if let Err(e) = write_atomic(&path, status) {
            log::error!("status: failed to write {}: {}", path.display(), e);
        }
    }
}

/// Atomic write: serializes to a temporary file next to the destination and
/// renames over it — readers never see half-written JSON.
pub fn write_atomic(path: &Path, status: &SyncStatus) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_vec_pretty(status).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_from(path: &Path) -> Result<SyncStatus, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("could not read {}: {}", path.display(), e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("invalid JSON in {}: {}", path.display(), e))
}

/// `Some(true/false)` if the process could be checked; `None` if the pid is invalid.
pub fn pid_alive(pid: u32) -> Option<bool> {
    if pid == 0 {
        return None;
    }
    Some(unsafe { libc::kill(pid as libc::pid_t, 0) } == 0)
}

/// Whether the daemon behind `s` is running: a live pid and no clean stop
/// recorded. Commands that touch the repository outside the daemon (no
/// shared lock) refuse while this holds.
pub fn daemon_running(s: &SyncStatus) -> bool {
    s.stopped_at.is_none() && pid_alive(s.pid) == Some(true)
}

fn format_ago(now: u64, then: u64) -> String {
    if then == 0 || then > now {
        return "unknown time".to_string();
    }
    let secs = now - then;
    if secs < 60 {
        format!("{} s ago", secs)
    } else if secs < 3600 {
        format!("{} min ago", secs / 60)
    } else {
        format!("{} h {} min ago", secs / 3600, (secs % 3600) / 60)
    }
}

/// Formats the status for the terminal. Pure: takes `now` and the result of
/// the pid check so it can be tested without a real process.
pub fn format_status(s: &SyncStatus, now: u64, alive: Option<bool>) -> String {
    let mut out = String::new();
    out.push_str(&format!("git-drive — branch {}\n", s.branch));
    out.push_str(&format!("  repo: {}\n", s.repo));
    let daemon_line = if let Some(stopped) = s.stopped_at {
        format!("stopped cleanly {}", format_ago(now, stopped))
    } else {
        match alive {
            Some(true) => format!("running (pid {})", s.pid),
            Some(false) => format!("POSSIBLY STOPPED (pid {} not responding)", s.pid),
            None => "unknown".to_string(),
        }
    };
    out.push_str(&format!("  daemon: {}\n", daemon_line));
    out.push_str(&format!(
        "  started {} | updated {}\n",
        format_ago(now, s.started_at),
        format_ago(now, s.updated_at)
    ));
    if alive == Some(true) && now.saturating_sub(s.updated_at) > 600 {
        out.push_str("  warning: no update in over 10 min — check the logs\n");
    }
    if let Some(sy) = &s.syncing {
        out.push_str(&format!(
            "  syncing: {} op(s) — {}, started {}\n",
            sy.ops,
            sy.stage,
            format_ago(now, sy.since)
        ));
    }
    out.push_str(&format!("  pending ops: {}\n", s.pending_ops));
    if s.unpushed > 0 {
        out.push_str(&format!(
            "  unpushed commits: {} (publishing in bounded batches)\n",
            s.unpushed
        ));
    }
    match &s.last_drain {
        Some(d) => {
            let commit = d.commit.as_deref().unwrap_or("none");
            let push = match d.pushed {
                Some(true) => ", push ok",
                Some(false) => ", PUSH FAILED",
                None => "",
            };
            out.push_str(&format!(
                "  last drain: {} — {} op(s), commit {}{}\n",
                format_ago(now, d.at),
                d.ops,
                &commit[..commit.len().min(8)],
                push
            ));
        }
        None => out.push_str("  last drain: none yet\n"),
    }
    match &s.p2p {
        None => out.push_str("  p2p: off\n"),
        Some(p) => {
            out.push_str(&format!(
                "  p2p: {} — this device: {} ({})\n",
                if p.online { "online" } else { "offline" },
                p.device_name,
                &p.device_id[..p.device_id.len().min(8)]
            ));
            for peer in &p.peers {
                let id = &peer.id[..peer.id.len().min(8)];
                let label = if peer.name.is_empty() {
                    id.to_string()
                } else {
                    format!("{} ({})", peer.name, id)
                };
                let state = match (peer.online, peer.last_seen) {
                    (true, _) => "online".to_string(),
                    (false, Some(at)) => format!("offline, seen {}", format_ago(now, at)),
                    (false, None) => "offline, never seen".to_string(),
                };
                out.push_str(&format!("    - {}: {}\n", label, state));
            }
        }
    }
    if let Some(reason) = &s.commit_blocked {
        out.push_str(&format!(
            "  COMMIT BLOCKED: {} — local writes kept in the journal\n",
            reason
        ));
    }
    match &s.last_pull {
        Some(p) => out.push_str(&format!(
            "  last pull: {} — {}\n",
            format_ago(now, p.at),
            p.outcome
        )),
        None => out.push_str("  last pull: none yet\n"),
    }
    if s.conflicts.is_empty() {
        out.push_str("  conflicts this session: none\n");
    } else {
        out.push_str(&format!("  conflicts this session ({}):\n", s.conflicts.len()));
        for c in &s.conflicts {
            out.push_str(&format!("    - {}\n", c));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample() -> SyncStatus {
        SyncStatus {
            pid: 42,
            repo: "/repo".into(),
            branch: "main".into(),
            started_at: 1_000,
            updated_at: 1_950,
            pending_ops: 2,
            last_drain: Some(DrainInfo {
                at: 1_900,
                ops: 3,
                commit: Some("abcdef0123456789".into()),
                pushed: Some(true),
            }),
            last_pull: Some(PullInfo {
                at: 1_940,
                outcome: "up to date".into(),
            }),
            conflicts: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn format_status_fresh_daemon() {
        let text = format_status(&sample(), 2_000, Some(true));
        assert!(text.contains("running (pid 42)"), "{}", text);
        assert!(text.contains("pending ops: 2"), "{}", text);
        assert!(text.contains("3 op(s), commit abcdef01, push ok"), "{}", text);
        assert!(text.contains("last pull: 60 s ago — up to date") || text.contains("last pull: 1 min ago — up to date"), "{}", text);
        assert!(text.contains("conflicts this session: none"), "{}", text);
        assert!(!text.contains("warning"), "{}", text);
    }

    #[test]
    fn format_status_dead_daemon_and_staleness() {
        let mut s = sample();
        s.updated_at = 100;
        let dead = format_status(&s, 2_000, Some(false));
        assert!(dead.contains("POSSIBLY STOPPED"), "{}", dead);
        // Staleness only warns while the daemon is alive
        let stale_alive = format_status(&s, 2_000, Some(true));
        assert!(stale_alive.contains("warning"), "{}", stale_alive);
    }

    #[test]
    fn format_status_shows_the_p2p_channel_and_its_peers() {
        let mut s = sample();
        let off = format_status(&s, 2_000, None);
        assert!(off.contains("p2p: off"), "{}", off);

        s.p2p = Some(P2pInfo {
            online: true,
            device_id: "ab".repeat(32),
            device_name: "mac".into(),
            peers: vec![
                PeerInfo { name: "allmight".into(), id: "cd".repeat(32), online: true, last_seen: Some(1_990) },
                PeerInfo { name: String::new(), id: "ef".repeat(32), online: false, last_seen: None },
            ],
        });
        let text = format_status(&s, 2_000, None);
        assert!(text.contains("p2p: online — this device: mac (abababab)"), "{}", text);
        assert!(text.contains("allmight (cdcdcdcd): online"), "{}", text);
        assert!(text.contains("efefefef: offline, never seen"), "{}", text);
    }

    #[test]
    fn daemon_running_needs_a_live_pid_and_no_clean_stop() {
        let mut s = sample();
        s.pid = std::process::id();
        s.stopped_at = None;
        assert!(daemon_running(&s), "this test process is alive");
        s.stopped_at = Some(1_000);
        assert!(!daemon_running(&s), "a recorded clean stop wins");
        s.stopped_at = None;
        s.pid = 0;
        assert!(!daemon_running(&s), "no pid recorded");
    }

    #[test]
    fn status_without_the_p2p_field_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let mut json = serde_json::to_value(sample()).unwrap();
        json.as_object_mut().unwrap().remove("p2p");
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(read_from(&path).unwrap().p2p.is_none());
    }

    #[test]
    fn a_drain_in_progress_shows_as_syncing_until_it_ends() {
        // The drain empties the queue as it starts: without this, a 3-minute
        // commit and push of a large file read as "pending ops: 0" and an
        // old "last drain" (2026-10-16).
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let state = StatusState::new();
        state.enable_file(path.clone());
        state.start_syncing(2);
        let s = read_from(&path).unwrap();
        let since = s.syncing.as_ref().expect("syncing published").since;
        let text = format_status(&s, since + 30, Some(true));
        assert!(text.contains("syncing: 2 op(s) — committing, started 30 s ago"), "{}", text);
        state.set_sync_stage("publishing");
        assert_eq!(read_from(&path).unwrap().syncing.unwrap().stage, "publishing");
        state.end_syncing();
        let s = read_from(&path).unwrap();
        assert!(s.syncing.is_none());
        assert!(!format_status(&s, since + 60, Some(true)).contains("syncing"));
    }

    #[test]
    fn status_without_the_syncing_field_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let mut json = serde_json::to_value(sample()).unwrap();
        json.as_object_mut().unwrap().remove("syncing");
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(read_from(&path).unwrap().syncing.is_none());
    }

    #[test]
    fn the_repository_size_is_published_against_its_capacity() {
        use crate::storage::{repo_capacity, GB};
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let state = StatusState::new();
        state.enable_file(path.clone());
        state.record_repo_size(GB, None);
        assert!(read_from(&path).unwrap().repo_usage.is_none(), "nothing to compare with before the boot sets the capacity");
        state.record_repo_size(5 * GB, Some(repo_capacity(Some("github.com"), None)));
        let u = read_from(&path).unwrap().repo_usage.expect("published");
        assert_eq!((u.used, u.capacity.limit, u.capacity.host.as_str()), (5 * GB, Some(10 * GB), "GitHub"));
        assert!(u.measured_at > 0);
        state.record_repo_size(6 * GB, None);
        let u = read_from(&path).unwrap().repo_usage.unwrap();
        assert_eq!((u.used, u.capacity.recommended), (6 * GB, Some(5 * GB)), "the capacity stays for the session");
    }

    #[test]
    fn status_without_the_repository_usage_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let mut json = serde_json::to_value(sample()).unwrap();
        json.as_object_mut().unwrap().remove("repo_usage");
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(read_from(&path).unwrap().repo_usage.is_none());
    }

    #[test]
    fn format_status_shows_a_blocked_commit_and_persists_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let state = StatusState::new();
        state.enable_file(path.clone());
        state.set_commit_blocked(Some("index tree does not match HEAD".into()));
        let loaded = read_from(&path).unwrap();
        let text = format_status(&loaded, 2_000, None);
        assert!(text.contains("COMMIT BLOCKED: index tree does not match HEAD"), "{}", text);

        state.set_commit_blocked(None);
        let text = format_status(&read_from(&path).unwrap(), 2_000, None);
        assert!(!text.contains("COMMIT BLOCKED"), "{}", text);
    }

    #[test]
    fn the_mount_is_recorded_and_a_new_boot_clears_it() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let state = StatusState::new();
        state.enable_file(path.clone());
        state.record_boot("/repo", "main");
        assert_eq!(read_from(&path).unwrap().mounted_at, None, "booting: not mounted yet");
        state.record_mounted();
        assert!(read_from(&path).unwrap().mounted_at.is_some());
        state.record_boot("/repo", "main");
        assert_eq!(read_from(&path).unwrap().mounted_at, None, "the next session starts unmounted");
        // Older status files have no such field.
        let old: SyncStatus = serde_json::from_str(
            r#"{"pid":1,"repo":"/r","branch":"main","started_at":1,"updated_at":1,"pending_ops":0,"last_drain":null,"last_pull":null,"conflicts":[]}"#,
        )
        .unwrap();
        assert_eq!(old.mounted_at, None);
    }

    #[test]
    fn format_status_before_any_drain_or_pull() {
        let s = SyncStatus {
            branch: "main".into(),
            ..Default::default()
        };
        let text = format_status(&s, 2_000, None);
        assert!(text.contains("last drain: none yet"), "{}", text);
        assert!(text.contains("last pull: none yet"), "{}", text);
    }

    #[test]
    fn write_atomic_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let status = SyncStatus {
            pid: 42,
            branch: "main".into(),
            last_drain: Some(DrainInfo {
                at: 100,
                ops: 3,
                commit: Some("abc123".into()),
                pushed: Some(true),
            }),
            ..Default::default()
        };
        write_atomic(&path, &status).unwrap();
        let loaded = read_from(&path).unwrap();
        assert_eq!(loaded.pid, 42);
        assert_eq!(loaded.branch, "main");
        assert_eq!(loaded.last_drain.unwrap().ops, 3);
    }

    #[test]
    fn read_from_missing_file_errors() {
        assert!(read_from(Path::new("/nonexistent/status.json")).is_err());
    }

    #[test]
    fn unpushed_roundtrips_and_defaults_to_zero() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let state = StatusState::new();
        state.enable_file(path.clone());
        state.set_unpushed(12);
        assert_eq!(read_from(&path).unwrap().unpushed, 12);

        // Old status.json without the field still parses (serde default).
        std::fs::write(&path, r#"{"pid":1,"repo":"r","branch":"main","started_at":0,"updated_at":0,"pending_ops":0,"last_drain":null,"last_pull":null,"conflicts":[]}"#).unwrap();
        assert_eq!(read_from(&path).unwrap().unpushed, 0);
    }

    #[test]
    fn format_status_shows_unpushed_when_positive() {
        let mut s = sample();
        s.unpushed = 5;
        let text = format_status(&s, 2_000, Some(true));
        assert!(text.contains("unpushed commits: 5"), "{}", text);
        s.unpushed = 0;
        let text = format_status(&s, 2_000, Some(true));
        assert!(!text.contains("unpushed"), "{}", text);
    }

    #[test]
    fn touch_refreshes_updated_at() {
        let state = StatusState::new();
        state.record_boot("/r", "main");
        let before = state.snapshot().updated_at;
        state.touch();
        assert!(state.snapshot().updated_at >= before);
    }

    #[test]
    fn status_state_records_events_and_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let state = StatusState::new();
        state.record_boot("/repo", "main");
        state.enable_file(path.clone());
        state.record_drain(2, Some("deadbeef".into()), Some(true), 0);
        state.record_pull("fast_forward");
        state.add_conflicts(vec!["a.txt.conflict-host-1".into()]);

        let loaded = read_from(&path).unwrap();
        assert_eq!(loaded.branch, "main");
        assert_eq!(loaded.last_drain.as_ref().unwrap().commit.as_deref(), Some("deadbeef"));
        assert_eq!(loaded.last_pull.as_ref().unwrap().outcome, "fast_forward");
        assert_eq!(loaded.conflicts, vec!["a.txt.conflict-host-1".to_string()]);
        assert!(loaded.updated_at >= loaded.started_at);
    }
}
