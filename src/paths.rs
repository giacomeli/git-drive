//! Path resolution for config, logs and status.
//!
//! Rule (decision 10 of Phase 2): without a flag, everything stays relative
//! to the CWD (compatible with the `cargo run` dev flow from the project
//! root). The standard macOS paths only come in via explicit flags — the
//! `git-drive install` command writes those flags into the LaunchAgent
//! plist, so the plist is the readable source of truth for where config,
//! log and status live.

use std::path::{Path, PathBuf};

pub(crate) fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `~/Library/Application Support/git-drive` (config + status when running via launchd).
pub fn app_support_dir() -> PathBuf {
    app_support_dir_from(&home_dir())
}

pub(crate) fn app_support_dir_from(home: &Path) -> PathBuf {
    home.join("Library/Application Support/git-drive")
}

/// Standard log directory of the installed daemon: `~/Library/Logs/git-drive`
/// on macOS, `<state dir>/logs` on Linux.
pub fn standard_log_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        standard_log_dir_from(&home_dir())
    }
    #[cfg(not(target_os = "macos"))]
    {
        state_dir().join("logs")
    }
}

/// Where the installed daemon keeps its state (status.json): Application
/// Support on macOS, `$XDG_STATE_HOME/git-drive` (default
/// `~/.local/state/git-drive`) on Linux.
pub fn state_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        app_support_dir()
    }
    #[cfg(not(target_os = "macos"))]
    {
        xdg_state_dir_from(&home_dir(), std::env::var_os("XDG_STATE_HOME").map(PathBuf::from))
    }
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn xdg_state_dir_from(home: &Path, xdg_state_home: Option<PathBuf>) -> PathBuf {
    xdg_state_home
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"))
        .join("git-drive")
}

/// Stable install location of the binary referenced by the login service.
/// The service must not point into a build tree: moving or renaming the
/// project directory broke the Mac LaunchAgent (exit 78, 2026-10).
pub fn installed_binary_path() -> PathBuf {
    home_dir().join(".local/bin/git-drive")
}

/// Installed Control Center binary (next to the daemon's).
pub fn installed_control_center_path() -> PathBuf {
    home_dir().join(".local/bin/git-drive-control-center")
}

/// Installed Control Center app bundle (macOS): Dock icon, Spotlight, app name.
pub fn control_center_app_path() -> PathBuf {
    home_dir().join("Applications/git-drive Control Center.app")
}

pub(crate) fn standard_log_dir_from(home: &Path) -> PathBuf {
    home.join("Library/Logs/git-drive")
}

/// `~/.config/git-drive` — canonical config + key directory (user decision
/// on 2026-07-06; also aligns with XDG for the future Linux port).
pub fn config_dir() -> PathBuf {
    config_dir_from(&home_dir())
}

pub(crate) fn config_dir_from(home: &Path) -> PathBuf {
    home.join(".config/git-drive")
}

pub fn standard_config_path() -> PathBuf {
    standard_config_path_from(&home_dir())
}

pub(crate) fn standard_config_path_from(home: &Path) -> PathBuf {
    config_dir_from(home).join("config.json")
}

/// Default path of the encryption key file (Phase 2.7).
pub fn standard_key_path() -> PathBuf {
    standard_key_path_from(&home_dir())
}

pub(crate) fn standard_key_path_from(home: &Path) -> PathBuf {
    config_dir_from(home).join("drive.key")
}

/// Old locations, kept as READ/migration fallbacks for existing
/// installations. Two generations: the project was named drive-sync until
/// 2026-07 (both dirs), and before 2026-07-06 config lived in Application
/// Support instead of `~/.config`.
fn legacy_xdg_config_dir() -> PathBuf {
    home_dir().join(".config/drive-sync")
}

fn legacy_app_support_dir() -> PathBuf {
    home_dir().join("Library/Application Support/drive-sync")
}

/// Legacy config locations, most recent generation first.
pub fn legacy_config_paths() -> Vec<PathBuf> {
    vec![
        legacy_xdg_config_dir().join("config.json"),
        legacy_app_support_dir().join("config.json"),
    ]
}

/// Legacy key locations, most recent generation first.
pub fn legacy_key_paths() -> Vec<PathBuf> {
    vec![
        legacy_xdg_config_dir().join("drive.key"),
        legacy_app_support_dir().join("drive.key"),
    ]
}

/// Config for READING: explicit flag > `./config.json` (dev flow) >
/// `~/.config/git-drive/config.json` > legacy locations (old names/dirs).
/// If none exists, the default is the XDG path (the "not found" message
/// points at the canonical location).
pub fn resolve_config_for_read(flag: Option<&str>) -> PathBuf {
    let mut candidates = vec![PathBuf::from("config.json"), standard_config_path()];
    candidates.extend(legacy_config_paths());
    resolve_config_for_read_from(flag, &candidates)
}

/// `candidates[1]` (the canonical XDG path) is the default when nothing exists.
fn resolve_config_for_read_from(flag: Option<&str>, candidates: &[PathBuf]) -> PathBuf {
    if let Some(f) = flag {
        return PathBuf::from(f);
    }
    for candidate in candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }
    candidates[1].clone()
}

/// Key for READING: explicit value from the config > XDG > legacy locations.
pub fn resolve_key_for_read(explicit: Option<&str>) -> PathBuf {
    let mut candidates = vec![standard_key_path()];
    candidates.extend(legacy_key_paths());
    resolve_key_for_read_from(explicit, &candidates)
}

fn resolve_key_for_read_from(explicit: Option<&str>, candidates: &[PathBuf]) -> PathBuf {
    if let Some(e) = explicit {
        return PathBuf::from(e);
    }
    for candidate in candidates {
        if candidate.exists() {
            return candidate.clone();
        }
    }
    candidates[0].clone()
}

pub fn standard_status_path() -> PathBuf {
    state_dir().join("status.json")
}

/// Status path for READING (`status` subcommand and tray):
/// explicit flag > `./logs/status.json` (dev flow) > platform standard.
pub fn resolve_status_for_read(flag: Option<&str>) -> PathBuf {
    resolve_status_for_read_from(flag, Path::new("logs/status.json"), &standard_status_path())
}

fn resolve_status_for_read_from(flag: Option<&str>, local: &Path, standard: &Path) -> PathBuf {
    if let Some(f) = flag {
        return PathBuf::from(f);
    }
    if local.exists() {
        return local.to_path_buf();
    }
    standard.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn standard_dirs_derive_from_home() {
        let home = Path::new("/Users/test");
        assert_eq!(
            app_support_dir_from(home),
            PathBuf::from("/Users/test/Library/Application Support/git-drive")
        );
        assert_eq!(
            standard_log_dir_from(home),
            PathBuf::from("/Users/test/Library/Logs/git-drive")
        );
        assert_eq!(
            config_dir_from(home),
            PathBuf::from("/Users/test/.config/git-drive")
        );
    }

    #[test]
    fn xdg_state_dir_honors_absolute_env_only() {
        let home = Path::new("/home/test");
        assert_eq!(
            xdg_state_dir_from(home, None),
            PathBuf::from("/home/test/.local/state/git-drive")
        );
        assert_eq!(
            xdg_state_dir_from(home, Some(PathBuf::from("/var/state"))),
            PathBuf::from("/var/state/git-drive")
        );
        // The spec says relative values are invalid and must be ignored.
        assert_eq!(
            xdg_state_dir_from(home, Some(PathBuf::from("rel"))),
            PathBuf::from("/home/test/.local/state/git-drive")
        );
    }

    #[test]
    fn config_and_key_defaults_live_in_xdg_config_dir() {
        let home = Path::new("/Users/test");
        assert_eq!(
            standard_config_path_from(home),
            PathBuf::from("/Users/test/.config/git-drive/config.json")
        );
        assert_eq!(
            standard_key_path_from(home),
            PathBuf::from("/Users/test/.config/git-drive/drive.key")
        );
    }

    #[test]
    fn resolve_config_order_flag_dev_xdg_legacies() {
        let dir = tempdir().unwrap();
        let dev = dir.path().join("config.json");
        let xdg = dir.path().join("xdg/config.json");
        let legacy_xdg = dir.path().join("legacy-xdg/config.json");
        let legacy_as = dir.path().join("legacy-as/config.json");
        let candidates = vec![
            dev.clone(),
            xdg.clone(),
            legacy_xdg.clone(),
            legacy_as.clone(),
        ];

        // flag wins over everything
        assert_eq!(
            resolve_config_for_read_from(Some("/x/c.json"), &candidates),
            PathBuf::from("/x/c.json")
        );
        // nothing exists -> default is the XDG path (the "not found" message
        // points at the new canonical location)
        assert_eq!(resolve_config_for_read_from(None, &candidates), xdg);
        // oldest legacy (Application Support) exists -> found
        std::fs::create_dir_all(legacy_as.parent().unwrap()).unwrap();
        std::fs::write(&legacy_as, b"{}").unwrap();
        assert_eq!(resolve_config_for_read_from(None, &candidates), legacy_as);
        // pre-rename XDG exists -> wins over Application Support
        std::fs::create_dir_all(legacy_xdg.parent().unwrap()).unwrap();
        std::fs::write(&legacy_xdg, b"{}").unwrap();
        assert_eq!(resolve_config_for_read_from(None, &candidates), legacy_xdg);
        // XDG exists -> XDG wins over every legacy
        std::fs::create_dir_all(xdg.parent().unwrap()).unwrap();
        std::fs::write(&xdg, b"{}").unwrap();
        assert_eq!(resolve_config_for_read_from(None, &candidates), xdg);
        // dev (CWD) exists -> dev wins (cargo run flow in the project)
        std::fs::write(&dev, b"{}").unwrap();
        assert_eq!(resolve_config_for_read_from(None, &candidates), dev);
    }

    #[test]
    fn resolve_key_order_explicit_xdg_legacies() {
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg/drive.key");
        let legacy = dir.path().join("legacy/drive.key");
        let candidates = vec![xdg.clone(), legacy.clone()];

        assert_eq!(
            resolve_key_for_read_from(Some("/k/drive.key"), &candidates),
            PathBuf::from("/k/drive.key")
        );
        assert_eq!(resolve_key_for_read_from(None, &candidates), xdg);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"k").unwrap();
        assert_eq!(resolve_key_for_read_from(None, &candidates), legacy);
        std::fs::create_dir_all(xdg.parent().unwrap()).unwrap();
        std::fs::write(&xdg, b"k").unwrap();
        assert_eq!(resolve_key_for_read_from(None, &candidates), xdg);
    }

    #[test]
    fn resolve_status_flag_wins() {
        let out = resolve_status_for_read_from(
            Some("/x/status.json"),
            Path::new("/does-not-exist/logs/status.json"),
            Path::new("/standard/status.json"),
        );
        assert_eq!(out, PathBuf::from("/x/status.json"));
    }

    #[test]
    fn resolve_status_prefers_existing_local_then_standard() {
        let dir = tempdir().unwrap();
        let local = dir.path().join("status.json");
        let standard = Path::new("/standard/status.json");

        // local does not exist -> standard
        assert_eq!(
            resolve_status_for_read_from(None, &local, standard),
            standard.to_path_buf()
        );
        // local exists -> local
        std::fs::write(&local, b"{}").unwrap();
        assert_eq!(resolve_status_for_read_from(None, &local, standard), local);
    }
}
