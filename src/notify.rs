//! macOS notifications via `osascript` (decision 3 of Phase 2 — zero
//! dependencies, no app bundle required). Persistent events are rate limited
//! (decision 12): first occurrence + at most one per hour while the condition
//! lasts; resolving the condition resets the limiter.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::status::now_unix;

/// Rate limit window for persistent events (1 hour).
const RATE_LIMIT_SECS: u64 = 3600;

pub const KEY_PULL_BLOCKED: &str = "pull_blocked";
pub const KEY_PUSH_FAILED: &str = "push_failed";
pub const KEY_COMMIT_FAILED: &str = "commit_failed";

#[derive(Debug, Clone)]
pub enum NotifyEvent {
    /// Merge created conflicted copies — the user needs to resolve them.
    Conflicts { copies: Vec<String> },
    /// Push + reconcile failed — local commits not published.
    PushFailed,
    /// Volume mounted and ready.
    Mounted { mount: String },
    /// Clean stop (SIGTERM/eject) completed.
    Stopped,
    /// Pull skipped due to dirty working tree (persistent, rate-limited).
    PullBlocked,
    /// The drain could not commit (persistent, rate-limited): writes are
    /// applied and journaled but not committed.
    CommitFailed,
}

impl NotifyEvent {
    /// Persistent events have a rate limit key; one-shot events are None.
    fn persistent_key(&self) -> Option<&'static str> {
        match self {
            NotifyEvent::PullBlocked => Some(KEY_PULL_BLOCKED),
            // Persistent too: while offline every drain and every pull cycle
            // retries the push — one notification per hour, not per minute.
            NotifyEvent::PushFailed => Some(KEY_PUSH_FAILED),
            NotifyEvent::CommitFailed => Some(KEY_COMMIT_FAILED),
            _ => None,
        }
    }
}

/// Notification title and body. Pure and testable.
pub fn format_event(event: &NotifyEvent) -> (String, String) {
    match event {
        NotifyEvent::Conflicts { copies } => {
            let list = copies
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if copies.len() > 3 {
                format!(" (+{})", copies.len() - 3)
            } else {
                String::new()
            };
            (
                "git-drive: merge conflict".to_string(),
                format!(
                    "Local version preserved at {}{}. The remote one became canonical.",
                    list, more
                ),
            )
        }
        NotifyEvent::PushFailed => (
            "git-drive: push failed".to_string(),
            "Changes committed locally but not published. Check network/credentials (git-drive status).".to_string(),
        ),
        NotifyEvent::Mounted { mount } => (
            "git-drive: drive mounted".to_string(),
            format!("Volume available at {}.", mount),
        ),
        NotifyEvent::Stopped => (
            "git-drive: drive unmounted".to_string(),
            "Clean stop completed — nothing left pending.".to_string(),
        ),
        NotifyEvent::PullBlocked => (
            "git-drive: pull blocked".to_string(),
            "Uncommitted local changes are blocking the pull (identity-less mode?). Sync is one-way for now.".to_string(),
        ),
        NotifyEvent::CommitFailed => (
            "git-drive: commit blocked".to_string(),
            "Local changes are saved but not committed or synced. See git-drive status and the log.".to_string(),
        ),
    }
}

/// Per-key rate limiter: fires on the first occurrence and then every
/// `window_secs` while the condition persists. `reset` when resolved.
pub struct RateLimiter {
    window_secs: u64,
    last_fired: Mutex<HashMap<&'static str, u64>>,
}

impl RateLimiter {
    pub fn new(window_secs: u64) -> Self {
        RateLimiter {
            window_secs,
            last_fired: Mutex::new(HashMap::new()),
        }
    }

    pub fn should_fire(&self, key: &'static str, now: u64) -> bool {
        let mut map = self.last_fired.lock();
        match map.get(key) {
            Some(last) if now.saturating_sub(*last) < self.window_secs => false,
            _ => {
                map.insert(key, now);
                true
            }
        }
    }

    pub fn reset(&self, key: &'static str) {
        self.last_fired.lock().remove(key);
    }
}

/// Notification dispatcher. Starts DISABLED — tests and embedded uses do not
/// notify; the real mount mode calls `enable()`.
pub struct Notifier {
    enabled: AtomicBool,
    limiter: RateLimiter,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier {
    pub fn new() -> Self {
        Notifier {
            enabled: AtomicBool::new(false),
            limiter: RateLimiter::new(RATE_LIMIT_SECS),
        }
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }

    /// Persistent condition resolved — the next occurrence notifies again.
    pub fn reset(&self, key: &'static str) {
        self.limiter.reset(key);
    }

    pub fn notify(&self, event: NotifyEvent) {
        if !self.enabled.load(Ordering::SeqCst) {
            return;
        }
        if let Some(key) = event.persistent_key() {
            if !self.limiter.should_fire(key, now_unix()) {
                return;
            }
        }
        let (title, body) = format_event(&event);
        dispatch_osascript(&title, &body);
    }
}

fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Waits for a fire-and-forget child on a background thread. Without the
/// wait, every spawned helper (osascript, xdg-open, the viewer) stays as a
/// zombie for the life of the parent — seen on the Arch tray (2026-10).
pub fn reap_in_background(child: std::io::Result<std::process::Child>) {
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(target_os = "macos")]
fn dispatch_osascript(title: &str, body: &str) {
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_escape(body),
        applescript_escape(title)
    );
    // Fire-and-forget: a notification failure must never affect the sync.
    let child = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    reap_in_background(child);
}

/// Linux: desktop notification via `notify-send` (libnotify). Absent binary
/// or no session bus = silently nothing, like a failed osascript.
#[cfg(not(target_os = "macos"))]
fn dispatch_osascript(title: &str, body: &str) {
    let child = std::process::Command::new("notify-send")
        .arg("--app-name=git-drive")
        .arg(title)
        .arg(body)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    reap_in_background(child);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_conflicts_truncates_list() {
        let copies: Vec<String> = (0..5).map(|i| format!("f{}.conflict-h-1", i)).collect();
        let (title, body) = format_event(&NotifyEvent::Conflicts { copies });
        assert!(title.contains("conflict"));
        assert!(body.contains("f0.conflict-h-1"));
        assert!(body.contains("(+2)"), "{}", body);
    }

    #[test]
    fn commit_failed_is_a_persistent_event_pointing_to_status() {
        let ev = NotifyEvent::CommitFailed;
        assert_eq!(ev.persistent_key(), Some(KEY_COMMIT_FAILED));
        let (title, body) = format_event(&ev);
        assert!(title.contains("commit"), "{}", title);
        assert!(body.contains("git-drive status"), "{}", body);
    }

    #[test]
    fn rate_limiter_first_fires_then_blocks_within_window() {
        let rl = RateLimiter::new(3600);
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 1_000));
        assert!(!rl.should_fire(KEY_PULL_BLOCKED, 1_500));
        assert!(!rl.should_fire(KEY_PULL_BLOCKED, 4_599));
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 4_600), "expired window must fire");
    }

    #[test]
    fn rate_limiter_reset_rearms_immediately() {
        let rl = RateLimiter::new(3600);
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 1_000));
        rl.reset(KEY_PULL_BLOCKED);
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 1_001), "reset must rearm");
    }

    #[test]
    fn notifier_disabled_never_rate_limits() {
        // Disabled does not consume the limiter window (neither notifies nor marks).
        let n = Notifier::new();
        n.notify(NotifyEvent::PullBlocked);
        assert!(
            n.limiter.should_fire(KEY_PULL_BLOCKED, now_unix()),
            "event with notifier disabled must not consume the window"
        );
    }

    #[test]
    fn applescript_escape_handles_quotes() {
        assert_eq!(applescript_escape(r#"a "b" \c"#), r#"a \"b\" \\c"#);
    }
}
