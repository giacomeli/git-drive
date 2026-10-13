//! Login-service installation shared by both platforms: the LaunchAgent
//! (`launchd.rs`, macOS) and the systemd user units (`systemd.rs`, Linux).
//! This module holds the platform-neutral steps — installing the binary at a
//! stable path, migrating legacy config/key locations and seeding the config.

use std::path::{Path, PathBuf};

use crate::paths;

/// `git-drive install`: dispatches to the platform's service manager.
pub fn install(repo: &str, mount: &str, with_tray: bool) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        crate::launchd::install(repo, mount, with_tray)
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::systemd::install(repo, mount, with_tray)
    }
}

/// `git-drive uninstall`: stops the daemon and removes the service.
pub fn uninstall() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        crate::launchd::uninstall()
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::systemd::uninstall()
    }
}

/// Copies the running executable to the stable install path
/// (`~/.local/bin/git-drive`) and returns that path — the service always
/// references the installed copy, never the build tree. Atomic (temp file +
/// rename), so re-installing over a running daemon is safe. No-op when the
/// running binary already is the installed one.
pub fn install_binary() -> anyhow::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let target = paths::installed_binary_path();
    install_binary_from(&current, &target)?;
    Ok(target)
}

pub(crate) fn install_binary_from(current: &Path, target: &Path) -> anyhow::Result<()> {
    let same = match (std::fs::canonicalize(current), std::fs::canonicalize(target)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("installing");
    std::fs::copy(current, &tmp)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, target)?;
    println!("binary: {} -> {}", current.display(), target.display());
    Ok(())
}

/// Moves config/key from a legacy location (pre-rename `drive-sync` dirs,
/// pre-2026-07-06 Application Support) to `~/.config/git-drive` when the
/// canonical file does not exist yet.
pub fn migrate_legacy_config() {
    let _ = std::fs::create_dir_all(paths::config_dir());
    for (legacies, new) in [
        (paths::legacy_config_paths(), paths::standard_config_path()),
        (paths::legacy_key_paths(), paths::standard_key_path()),
    ] {
        if new.exists() {
            continue;
        }
        if let Some(legacy) = legacies.iter().find(|p| p.exists()) {
            match std::fs::rename(legacy, &new) {
                Ok(_) => println!("migrated: {} -> {}", legacy.display(), new.display()),
                Err(e) => eprintln!("warning: failed to migrate {}: {}", legacy.display(), e),
            }
        }
    }
}

/// Seeds the canonical config from `./config.json` (dev flow) when it does
/// not exist yet; otherwise warns — without an identity there are no commits.
/// Returns the canonical config path.
pub fn ensure_config() -> anyhow::Result<PathBuf> {
    let config = paths::standard_config_path();
    if !config.exists() {
        if Path::new("config.json").exists() {
            std::fs::copy("config.json", &config)?;
            println!("config: ./config.json copied to {}", config.display());
        } else {
            eprintln!(
                "warning: no config — create {} with user_name/user_email to enable commits",
                config.display()
            );
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_binary_copies_atomically_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("build/git-drive");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"binary-v1").unwrap();
        let target = dir.path().join("home/.local/bin/git-drive");

        install_binary_from(&src, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"binary-v1");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        assert!(!target.with_extension("installing").exists());

        // Re-install from the installed copy itself: no-op, no self-truncation.
        install_binary_from(&target, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"binary-v1");

        // Upgrade overwrites.
        std::fs::write(&src, b"binary-v2").unwrap();
        install_binary_from(&src, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"binary-v2");
    }
}
