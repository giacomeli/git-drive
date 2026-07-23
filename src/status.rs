//! Observable daemon state, written to `logs/status.json` and read by the
//! `git-drive status` subcommand. Timestamps are unix seconds (u64) — human
//! formatting happens only in the CLI (decision 10 of Phase 1).

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub pending_ops: usize,
    /// Local commits still awaiting publication to the remote (progressive
    /// push catch-up in flight or interrupted). 0 = remote at the local tip.
    #[serde(default)]
    pub unpushed: usize,
    pub last_drain: Option<DrainInfo>,
    pub last_pull: Option<PullInfo>,
    /// Conflicted copies created in this session (paths of the `*.conflict-*` copies).
    pub conflicts: Vec<String>,
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
        // A new session invalidates the previous one's shutdown (same status file).
        s.stopped_at = None;
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
