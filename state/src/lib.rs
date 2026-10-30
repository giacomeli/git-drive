//! The git-drive daemon's observable state, shared by the daemon and its
//! apps: `status.json` and its types, where git-drive keeps its files, and
//! disk figures. No FUSE and no Git here, so an app that only reads the
//! state (UnniFiles, the file manager) links neither macFUSE nor libgit2 and opens on a
//! Mac without them. The `git-drive` library re-exports these modules under
//! their old paths (`git_drive::status`, `git_drive::paths`, `git_drive::storage`).

pub mod paths;
pub mod status;
pub mod storage;

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
