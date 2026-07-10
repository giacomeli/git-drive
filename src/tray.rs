//! Menu bar status icon (`drive-sync tray`).
//!
//! Separate process from the daemon (decision 1 of Phase 2): polls
//! `status.json` and reflects the state in the icon and menu. Crates
//! `tray-icon` + `tao` (decision 2) — ready for the future Linux port
//! (appindicator).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::status::{self, SyncStatus};

/// Polling interval for status.json.
const POLL_SECS: u64 = 2;
/// `updated_at` older than this (with a 60s pull, 3 cycles) = stale.
const STALE_SECS: u64 = 180;

/// Visual state of the icon, from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    /// Synced, nothing pending.
    Ok,
    /// Pending ops / activity in progress.
    Busy,
    /// Conflicts or failed push — needs attention.
    Attention,
    /// Daemon stopped, dead, or stale status.
    Stopped,
}

impl TrayState {
    fn label(self) -> &'static str {
        match self {
            TrayState::Ok => "synced",
            TrayState::Busy => "syncing",
            TrayState::Attention => "attention",
            TrayState::Stopped => "stopped",
        }
    }

    /// RGBA of the icon circle (macOS system colors).
    fn color(self) -> [u8; 4] {
        match self {
            TrayState::Ok => [52, 199, 89, 255],
            TrayState::Busy => [255, 159, 10, 255],
            TrayState::Attention => [255, 69, 58, 255],
            TrayState::Stopped => [142, 142, 147, 255],
        }
    }
}

/// Maps the status read (or its absence) to a state + summary lines.
/// Pure and testable — the event loop only renders what comes out of here.
pub fn evaluate(
    status: Option<&SyncStatus>,
    now: u64,
    alive: Option<bool>,
) -> (TrayState, Vec<String>) {
    let Some(s) = status else {
        return (
            TrayState::Stopped,
            vec!["no status available — has the drive been mounted yet?".to_string()],
        );
    };

    let mut lines = Vec::new();
    lines.push(format!("branch {} — {}", s.branch, s.repo));

    let state = if s.stopped_at.is_some()
        || alive == Some(false)
        || now.saturating_sub(s.updated_at) > STALE_SECS
    {
        lines.push("daemon stopped".to_string());
        TrayState::Stopped
    } else if !s.conflicts.is_empty()
        || s.last_drain.as_ref().is_some_and(|d| d.pushed == Some(false))
    {
        if !s.conflicts.is_empty() {
            lines.push(format!("{} conflict(s) to resolve", s.conflicts.len()));
        }
        if s.last_drain.as_ref().is_some_and(|d| d.pushed == Some(false)) {
            lines.push("push failed — changes not published".to_string());
        }
        TrayState::Attention
    } else if s.pending_ops > 0 {
        lines.push(format!("{} pending op(s)", s.pending_ops));
        TrayState::Busy
    } else {
        match &s.last_drain {
            Some(d) => lines.push(format!("last sync {} s ago", now.saturating_sub(d.at))),
            None => lines.push("no sync yet in this session".to_string()),
        }
        TrayState::Ok
    };
    if let Some(p) = &s.last_pull {
        lines.push(format!("pull: {}", p.outcome));
    }
    (state, lines)
}

/// Filled circle with simple antialiasing, for `Icon::from_rgba`.
fn icon_rgba(color: [u8; 4], size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let center = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 * 0.38;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = ((radius + 0.8 - dist) / 1.6).clamp(0.0, 1.0);
            out.extend_from_slice(&[
                color[0],
                color[1],
                color[2],
                (color[3] as f32 * alpha) as u8,
            ]);
        }
    }
    out
}

fn make_icon(state: TrayState) -> Icon {
    const SIZE: u32 = 22;
    Icon::from_rgba(icon_rgba(state.color(), SIZE), SIZE, SIZE).expect("valid RGBA icon")
}

/// Opens `path` in the system's default file manager: `open` on macOS,
/// `xdg-open` on Linux (honors whatever file manager the desktop registered).
fn open_in_file_manager(path: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Spawns the egui viewer (status + live log tail) as a separate process.
/// A separate process avoids clashing with the tray's own `tao` event loop and
/// works cross-platform, replacing the macOS-only `osascript`/Terminal hack.
/// Uses the current executable so it does not depend on PATH.
fn spawn_viewer(status_path: &Path) {
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("drive-sync"));
    let _ = std::process::Command::new(bin)
        .arg("viewer")
        .arg("--file")
        .arg(status_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Starts the tray. BLOCKS the main thread with tao's event loop (an AppKit
/// requirement) — which is why the dispatch in main happens before the tokio
/// runtime.
pub fn run(status_path: PathBuf) -> anyhow::Result<()> {
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::new().build();
    // No Dock icon: the tray is a menu bar accessory.
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }

    // Menu: summary lines (disabled) + actions.
    let header = MenuItem::new("drive-sync", false, None);
    let line1 = MenuItem::new("loading status...", false, None);
    let line2 = MenuItem::new("", false, None);
    let line3 = MenuItem::new("", false, None);
    let open_drive = MenuItem::new("Open drive in file manager", true, None);
    let viewer = MenuItem::new("Status & log window", true, None);
    let quit = MenuItem::new("Quit tray", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &header,
        &line1,
        &line2,
        &line3,
        &PredefinedMenuItem::separator(),
        &open_drive,
        &viewer,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;

    let mut tray: Option<TrayIcon> = None;
    let mut last_poll = Instant::now() - Duration::from_secs(POLL_SECS);
    let mut current: Option<SyncStatus> = None;

    let menu_channel = MenuEvent::receiver();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));

        // Icon creation must happen with the event loop already active on macOS.
        if tray.is_none() {
            if let tao::event::Event::NewEvents(tao::event::StartCause::Init) = event {
                tray = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_icon(make_icon(TrayState::Stopped))
                        .with_tooltip("drive-sync")
                        .build()
                        .expect("create tray icon"),
                );
            }
        }

        // Status polling.
        if tray.is_some() && last_poll.elapsed() >= Duration::from_secs(POLL_SECS) {
            last_poll = Instant::now();
            current = status::read_from(Path::new(&status_path)).ok();
            let alive = current.as_ref().and_then(|s| status::pid_alive(s.pid));
            let (state, lines) = evaluate(current.as_ref(), status::now_unix(), alive);

            header.set_text(format!("drive-sync — {}", state.label()));
            line1.set_text(lines.first().cloned().unwrap_or_default());
            line2.set_text(lines.get(1).cloned().unwrap_or_default());
            line3.set_text(lines.get(2).cloned().unwrap_or_default());
            if let Some(t) = &tray {
                let _ = t.set_icon(Some(make_icon(state)));
                let _ = t.set_tooltip(Some(format!("drive-sync — {}", state.label())));
            }
        }

        // Menu actions.
        while let Ok(ev) = menu_channel.try_recv() {
            if ev.id == open_drive.id() {
                if let Some(s) = &current {
                    if !s.mount.is_empty() {
                        open_in_file_manager(&s.mount);
                    }
                }
            } else if ev.id == viewer.id() {
                spawn_viewer(&status_path);
            } else if ev.id == quit.id() {
                *control_flow = ControlFlow::Exit;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{DrainInfo, PullInfo};

    fn base_status() -> SyncStatus {
        SyncStatus {
            pid: 42,
            repo: "/repo".into(),
            branch: "main".into(),
            mount: "/GitDrive".into(),
            updated_at: 1_000,
            last_drain: Some(DrainInfo {
                at: 990,
                ops: 1,
                commit: Some("abc".into()),
                pushed: Some(true),
            }),
            last_pull: Some(PullInfo {
                at: 995,
                outcome: "up to date".into(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn evaluate_ok_when_synced_and_alive() {
        let s = base_status();
        let (state, lines) = evaluate(Some(&s), 1_010, Some(true));
        assert_eq!(state, TrayState::Ok);
        assert!(lines.iter().any(|l| l.contains("last sync")));
    }

    #[test]
    fn evaluate_busy_with_pending_ops() {
        let mut s = base_status();
        s.pending_ops = 3;
        let (state, _) = evaluate(Some(&s), 1_010, Some(true));
        assert_eq!(state, TrayState::Busy);
    }

    #[test]
    fn evaluate_attention_on_conflicts_or_failed_push() {
        let mut s = base_status();
        s.conflicts = vec!["a.conflict-h-1".into()];
        assert_eq!(evaluate(Some(&s), 1_010, Some(true)).0, TrayState::Attention);

        let mut s2 = base_status();
        s2.last_drain.as_mut().unwrap().pushed = Some(false);
        assert_eq!(evaluate(Some(&s2), 1_010, Some(true)).0, TrayState::Attention);
    }

    #[test]
    fn evaluate_stopped_on_dead_stale_or_clean_stop() {
        let s = base_status();
        assert_eq!(evaluate(Some(&s), 1_010, Some(false)).0, TrayState::Stopped);
        assert_eq!(evaluate(Some(&s), 1_000 + STALE_SECS + 1, Some(true)).0, TrayState::Stopped);
        let mut s2 = base_status();
        s2.stopped_at = Some(1_005);
        assert_eq!(evaluate(Some(&s2), 1_010, Some(true)).0, TrayState::Stopped);
        assert_eq!(evaluate(None, 1_010, None).0, TrayState::Stopped);
    }

    #[test]
    fn icon_rgba_has_expected_size_and_center_alpha() {
        let px = icon_rgba([255, 0, 0, 255], 22);
        assert_eq!(px.len(), 22 * 22 * 4);
        // opaque center, transparent corner
        let center_idx = ((11 * 22 + 11) * 4 + 3) as usize;
        assert_eq!(px[center_idx], 255);
        assert_eq!(px[3], 0);
    }
}
