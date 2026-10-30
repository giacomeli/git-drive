//! Menu bar status icon (`git-drive tray`).
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
    /// The remote does not answer: changes stay on this device until it does.
    Offline,
    /// Conflicts, a failed push or merge, a blocked commit — needs attention.
    Attention,
    /// Daemon stopped, dead, or stale status.
    Stopped,
}

impl TrayState {
    fn label(self) -> &'static str {
        match self {
            TrayState::Ok => "synced",
            TrayState::Busy => "syncing",
            TrayState::Offline => "offline",
            TrayState::Attention => "attention",
            TrayState::Stopped => "stopped",
        }
    }

    /// The status dot: macOS's system colors, in their dark or light variant.
    fn dot_color(self, dark: bool) -> [u8; 3] {
        match (self, dark) {
            (TrayState::Ok, true) => [48, 209, 88],
            (TrayState::Ok, false) => [52, 199, 89],
            (TrayState::Busy, true) => [10, 132, 255],
            (TrayState::Busy, false) => [0, 122, 255],
            (TrayState::Offline, true) => [255, 159, 10],
            (TrayState::Offline, false) => [255, 149, 0],
            (TrayState::Attention, true) => [255, 69, 58],
            (TrayState::Attention, false) => [255, 59, 48],
            (TrayState::Stopped, true) => [152, 152, 157],
            (TrayState::Stopped, false) => [142, 142, 147],
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

    // A fetch that fails is the network; a push that fails meanwhile is the
    // same outage. Any other pull error is the drive's own problem.
    let pull = s.last_pull.as_ref().map(|p| p.outcome.as_str()).unwrap_or("");
    let network_down = pull.starts_with("fetch error");
    let pull_failed = !network_down && pull.contains("error");
    let push_failed = !network_down && s.last_drain.as_ref().is_some_and(|d| d.pushed == Some(false));

    let state = if s.stopped_at.is_some()
        || alive == Some(false)
        || now.saturating_sub(s.updated_at) > STALE_SECS
    {
        lines.push("daemon stopped".to_string());
        TrayState::Stopped
    } else if !s.conflicts.is_empty() || push_failed || pull_failed || s.commit_blocked.is_some() {
        if !s.conflicts.is_empty() {
            lines.push(format!("{} conflict(s) to resolve", s.conflicts.len()));
        }
        if push_failed {
            lines.push("push failed — changes not published".to_string());
        }
        if s.commit_blocked.is_some() {
            lines.push("commit blocked — local writes kept in the journal".to_string());
        }
        if pull_failed {
            lines.push(format!("pull failed: {}", pull));
        }
        TrayState::Attention
    } else if network_down {
        lines.push("remote unreachable — changes stay on this device until it answers".to_string());
        TrayState::Offline
    } else if s.syncing.is_some() || s.pending_ops > 0 || s.unpushed > 0 {
        if let Some(sy) = &s.syncing {
            lines.push(format!("syncing {} change(s) — {}", sy.ops, sy.stage));
        }
        if s.pending_ops > 0 {
            lines.push(format!("{} pending op(s)", s.pending_ops));
        }
        if s.unpushed > 0 {
            lines.push(format!("{} commit(s) waiting to publish", s.unpushed));
        }
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

/// Lucide's folder-git-2 (the app's mark, as in the Control Center) on its
/// 24-unit grid, as straight segments: the folder outline, the branch's curve
/// and stem, and the ring of its upper node. Its lower node is the status dot.
fn mark_segments() -> Vec<((f32, f32), (f32, f32))> {
    fn arc(c: (f32, f32), r: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
        const STEPS: usize = 12;
        (0..=STEPS)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / STEPS as f32).to_radians();
                (c.0 + r * a.cos(), c.1 + r * a.sin())
            })
            .collect()
    }
    // Paths as point runs (the y axis points down, as in SVG).
    let runs: Vec<Vec<(f32, f32)>> = vec![
        // The branch: from the lower node, curving up to the upper one, then the stem.
        [arc((18.0, 14.0), 5.0, 90.0, 180.0), vec![(13.0, 22.0)]].concat(),
        // The folder, open where the branch leaves it.
        [
            vec![(9.0, 20.0)],
            arc((4.0, 18.0), 2.0, 90.0, 180.0),
            arc((4.0, 5.0), 2.0, 180.0, 270.0),
            arc((7.9, 5.0), 2.0, 270.0, 326.9),
            arc((12.07, 4.0), 2.0, 146.6, 90.0),
            arc((20.0, 8.0), 2.0, 270.0, 360.0),
            vec![(22.0, 13.0)],
        ]
        .concat(),
        // The upper node.
        arc((13.0, 12.0), 2.0, 0.0, 360.0),
    ];
    runs.iter().flat_map(|run| run.windows(2).map(|w| (w[0], w[1]))).collect()
}

fn segment_distance(p: (f32, f32), (a, b): ((f32, f32), (f32, f32))) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) };
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

/// The menu bar icon as RGBA, `size` pixels square: the app's mark in white
/// on a dark bar (black, as macOS draws its own, on a light one), dimmed while
/// the daemon is stopped, with the branch's lower node filled in the state's
/// color and a clear ring around it so the dot reads on its own. Drawn by
/// distance to each stroke: antialiased edges at any size, no image files.
fn icon_rgba(state: TrayState, dark: bool, size: u32) -> Vec<u8> {
    const HALF_STROKE: f32 = 1.0; // lucide's 2-unit stroke
    const NODE: (f32, f32) = (20.0, 19.0);
    const DOT: f32 = 3.3; // a little over the stroked node (3)
    const GAP: f32 = 1.3;
    let segments = mark_segments();
    let unit = 24.0 / size as f32; // one pixel, in grid units
    // Coverage of a pixel whose center lies `inside` units within an edge.
    let cover = |inside: f32| (inside / unit + 0.5).clamp(0.0, 1.0);
    let ink: [f32; 3] = if dark { [255.0; 3] } else { [0.0; 3] };
    let ink_alpha = match (state, dark) {
        (TrayState::Stopped, _) => 0.45,
        (_, true) => 1.0,
        (_, false) => 0.85,
    };
    let dot = state.dot_color(dark).map(f32::from);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let p = ((x as f32 + 0.5) * unit, (y as f32 + 0.5) * unit);
            let to_node = ((p.0 - NODE.0).powi(2) + (p.1 - NODE.1).powi(2)).sqrt();
            let dot_a = cover(DOT - to_node);
            let stroke = segments.iter().map(|&s| segment_distance(p, s)).fold(f32::MAX, f32::min);
            let ink_a = cover(HALF_STROKE - stroke) * cover(to_node - DOT - GAP) * ink_alpha * (1.0 - dot_a);
            let alpha = dot_a + ink_a;
            if alpha <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            let mix = |c: usize| ((dot[c] * dot_a + ink[c] * ink_a) / alpha).round() as u8;
            out.extend_from_slice(&[mix(0), mix(1), mix(2), (alpha * 255.0).round() as u8]);
        }
    }
    out
}

/// Version + build identifier of the running binary, for the tray menu.
/// `GIT_DRIVE_BUILD` is injected by `build.rs`.
fn build_line() -> String {
    format!(
        "build v{} · {}",
        env!("CARGO_PKG_VERSION"),
        env!("GIT_DRIVE_BUILD")
    )
}

/// Whether the menu bar behind the icon is dark: the status item's own
/// appearance, which on recent macOS follows the wallpaper as well as the
/// system's Light or Dark setting.
#[cfg(target_os = "macos")]
fn menu_bar_is_dark(tray: &TrayIcon) -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua};
    use objc2_foundation::NSArray;
    let (Some(mtm), Some(item)) = (MainThreadMarker::new(), tray.ns_status_item()) else {
        return true;
    };
    let Some(button) = item.button(mtm) else { return true };
    // SAFETY: AppKit's appearance-name constants, read on the main thread.
    let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
    let names = NSArray::from_slice(&[aqua, dark]);
    button
        .effectiveAppearance()
        .bestMatchFromAppearancesWithNames(&names)
        .is_some_and(|name| name.isEqualToString(dark))
}

/// GNOME's top bar is dark in both of its styles.
#[cfg(not(target_os = "macos"))]
fn menu_bar_is_dark(_tray: &TrayIcon) -> bool {
    true
}

/// 44 pixels: the menu bar draws the icon 18-22 points tall, sharp on a 2x screen.
fn make_icon(state: TrayState, dark: bool) -> Icon {
    const SIZE: u32 = 44;
    Icon::from_rgba(icon_rgba(state, dark, SIZE), SIZE, SIZE).expect("valid RGBA icon")
}

/// Opens `path` in the system's default file manager: `open` on macOS,
/// `xdg-open` on Linux (honors whatever file manager the desktop registered).
fn open_in_file_manager(path: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(not(target_os = "macos"))]
    let program = "xdg-open";
    let child = std::process::Command::new(program)
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    crate::notify::reap_in_background(child);
}

/// Spawns the egui viewer (status + live log tail) as a separate process.
/// A separate process avoids clashing with the tray's own `tao` event loop and
/// works cross-platform, replacing the macOS-only `osascript`/Terminal hack.
/// Uses the current executable so it does not depend on PATH.
fn spawn_viewer(status_path: &Path) {
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git-drive"));
    let child = std::process::Command::new(bin)
        .arg("viewer")
        .arg("--file")
        .arg(status_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    crate::notify::reap_in_background(child);
}

/// Opens the Control Center at `page`; without it installed, the egui status
/// viewer (status + live log tail) takes its place.
fn open_control_center(page: Option<&str>, status_path: &Path) {
    match crate::control::open_control_center(page) {
        Ok(true) => {}
        Ok(false) => spawn_viewer(status_path),
        Err(e) => {
            log::warn!("tray: control center: {}", e);
            spawn_viewer(status_path);
        }
    }
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
    let header = MenuItem::new("git-drive", false, None);
    let build = MenuItem::new(build_line(), false, None);
    let line1 = MenuItem::new("loading status...", false, None);
    let line2 = MenuItem::new("", false, None);
    let line3 = MenuItem::new("", false, None);
    let open_drive = MenuItem::new("Open drive in file manager", true, None);
    let center = MenuItem::new("Open Control Center", true, None);
    let viewer = MenuItem::new("Show log", true, None);
    let quit = MenuItem::new("Quit tray", true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &header,
        &build,
        &line1,
        &line2,
        &line3,
        &PredefinedMenuItem::separator(),
        &open_drive,
        &center,
        &viewer,
        &PredefinedMenuItem::separator(),
        &quit,
    ])?;

    let mut tray: Option<TrayIcon> = None;
    let mut last_poll = Instant::now() - Duration::from_secs(POLL_SECS);
    let mut current: Option<SyncStatus> = None;
    // What the icon shows (state, dark bar): redrawn only when it changes.
    let mut drawn: Option<(TrayState, bool)> = None;

    let menu_channel = MenuEvent::receiver();

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));

        // Icon creation must happen with the event loop already active on macOS.
        if tray.is_none() {
            if let tao::event::Event::NewEvents(tao::event::StartCause::Init) = event {
                tray = Some(
                    TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_icon(make_icon(TrayState::Stopped, true))
                        .with_tooltip("git-drive")
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

            header.set_text(format!("git-drive — {}", state.label()));
            line1.set_text(lines.first().cloned().unwrap_or_default());
            line2.set_text(lines.get(1).cloned().unwrap_or_default());
            line3.set_text(lines.get(2).cloned().unwrap_or_default());
            if let Some(t) = &tray {
                let look = (state, menu_bar_is_dark(t));
                if drawn != Some(look) {
                    drawn = Some(look);
                    let _ = t.set_icon(Some(make_icon(look.0, look.1)));
                }
                let _ = t.set_tooltip(Some(format!("git-drive — {}", state.label())));
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
            } else if ev.id == center.id() {
                open_control_center(None, &status_path);
            } else if ev.id == viewer.id() {
                open_control_center(Some("logs"), &status_path);
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
    fn evaluate_busy_while_a_drain_runs() {
        // The queue is empty while a drain works (it took the changes): the
        // drain itself must show as busy.
        let mut s = base_status();
        s.syncing = Some(crate::status::SyncingInfo { since: 1_000, ops: 1, stage: "publishing".into() });
        let (state, lines) = evaluate(Some(&s), 1_010, Some(true));
        assert_eq!(state, TrayState::Busy);
        assert!(lines.iter().any(|l| l.contains("syncing 1 change(s)") && l.contains("publishing")), "{:?}", lines);
    }

    #[test]
    fn evaluate_busy_with_pending_ops() {
        let mut s = base_status();
        s.pending_ops = 3;
        let (state, _) = evaluate(Some(&s), 1_010, Some(true));
        assert_eq!(state, TrayState::Busy);
    }

    #[test]
    fn evaluate_busy_with_unpushed_commits() {
        let mut s = base_status();
        s.unpushed = 7;
        let (state, lines) = evaluate(Some(&s), 1_010, Some(true));
        assert_eq!(state, TrayState::Busy);
        assert!(lines.iter().any(|l| l.contains("7 commit(s) waiting to publish")));
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
    fn evaluate_offline_when_the_remote_is_unreachable() {
        // A fetch that fails is the network, not the drive: changes stay on
        // this device and go out once the remote answers again.
        let mut s = base_status();
        s.last_pull.as_mut().unwrap().outcome = "fetch error".into();
        let (state, lines) = evaluate(Some(&s), 1_010, Some(true));
        assert_eq!(state, TrayState::Offline);
        assert!(lines.iter().any(|l| l.contains("remote unreachable")), "{:?}", lines);
        // A push that failed meanwhile is the same outage, not an alarm.
        s.last_drain.as_mut().unwrap().pushed = Some(false);
        assert_eq!(evaluate(Some(&s), 1_010, Some(true)).0, TrayState::Offline);
    }

    #[test]
    fn evaluate_attention_on_a_blocked_commit_or_a_failed_merge() {
        let mut s = base_status();
        s.commit_blocked = Some("index tree does not match HEAD".into());
        assert_eq!(evaluate(Some(&s), 1_010, Some(true)).0, TrayState::Attention);
        let mut s = base_status();
        s.last_pull.as_mut().unwrap().outcome = "merge error".into();
        assert_eq!(evaluate(Some(&s), 1_010, Some(true)).0, TrayState::Attention);
    }

    /// The pixel at a point of the 24-unit drawing (the lucide grid).
    fn pixel(rgba: &[u8], size: u32, ux: f32, uy: f32) -> [u8; 4] {
        let x = (ux / 24.0 * size as f32) as u32;
        let y = (uy / 24.0 * size as f32) as u32;
        let i = ((y * size + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn the_mark_is_white_on_a_dark_menu_bar_and_black_on_a_light_one() {
        const SIZE: u32 = 44;
        // On the folder's left edge (x = 2 units).
        let dark = icon_rgba(TrayState::Ok, true, SIZE);
        assert_eq!(dark.len(), (SIZE * SIZE * 4) as usize);
        let p = pixel(&dark, SIZE, 2.0, 12.0);
        assert_eq!(&p[..3], &[255, 255, 255]);
        assert!(p[3] > 200, "{:?}", p);
        let light = icon_rgba(TrayState::Ok, false, SIZE);
        let p = pixel(&light, SIZE, 2.0, 12.0);
        assert_eq!(&p[..3], &[0, 0, 0]);
        assert!(p[3] > 180, "{:?}", p);
        // Background stays clear.
        assert_eq!(pixel(&dark, SIZE, 21.8, 1.6)[3], 0);
    }

    #[test]
    fn the_branch_node_at_the_lower_right_carries_the_state() {
        const SIZE: u32 = 44;
        let at_node = |state, dark| pixel(&icon_rgba(state, dark, SIZE), SIZE, 20.0, 19.0);
        assert_eq!(at_node(TrayState::Ok, true), [48, 209, 88, 255]);
        assert_eq!(at_node(TrayState::Ok, false), [52, 199, 89, 255]);
        assert_eq!(&at_node(TrayState::Busy, true)[..3], &[10, 132, 255]);
        assert_eq!(&at_node(TrayState::Offline, true)[..3], &[255, 159, 10]);
        assert_eq!(&at_node(TrayState::Attention, true)[..3], &[255, 69, 58]);
        assert_eq!(&at_node(TrayState::Stopped, true)[..3], &[152, 152, 157]);
        // A clear ring cuts the branch's curve around the node, so the dot
        // reads on its own at menu-bar size.
        let ok = icon_rgba(TrayState::Ok, true, SIZE);
        assert_eq!(pixel(&ok, SIZE, 16.09, 18.27)[3], 0, "the curve stops short of the dot");
    }

    #[test]
    fn a_stopped_daemon_dims_the_mark() {
        const SIZE: u32 = 44;
        let ok = pixel(&icon_rgba(TrayState::Ok, true, SIZE), SIZE, 2.0, 12.0)[3];
        let stopped = pixel(&icon_rgba(TrayState::Stopped, true, SIZE), SIZE, 2.0, 12.0)[3];
        assert!(stopped < ok / 2 + 20, "stopped {} vs ok {}", stopped, ok);
    }
}
