//! Login-service installation shared by both platforms: the LaunchAgent
//! (`launchd.rs`, macOS) and the systemd user units (`systemd.rs`, Linux).
//! This module holds the platform-neutral steps — installing the binary at a
//! stable path, migrating legacy config/key locations and seeding the config.

use std::path::{Path, PathBuf};

use crate::paths;

/// `git-drive install`: dispatches to the platform's service manager.
pub fn install(repo: &str, mount: &str, with_tray: bool) -> anyhow::Result<()> {
    // Before the service: the tray it starts opens the Control Center.
    if let Err(e) = install_control_center() {
        eprintln!("control center: not installed: {}", e);
    }
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

/// Installs the Control Center when it was built next to this executable
/// (`npm run tauri build` in `control-center/` puts it in `target/release`):
/// the binary at `~/.local/bin/git-drive-control-center`, plus the app bundle
/// in `~/Applications` on macOS or a launcher entry and icon on Linux.
/// Optional: without a build, the tray keeps the egui status viewer.
pub fn install_control_center() -> anyhow::Result<Option<PathBuf>> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap_or(Path::new("."));
    let built = dir.join("git-drive-control-center");
    if !built.is_file() {
        println!("control center: not built, skipped (cd control-center && npm install && npm run tauri build)");
        return Ok(None);
    }
    let target = paths::installed_control_center_path();
    install_binary_from(&built, &target)?;
    #[cfg(target_os = "macos")]
    {
        let bundle = dir.join("bundle/macos/git-drive Control Center.app");
        if bundle.is_dir() {
            let app = paths::control_center_app_path();
            if let Some(parent) = app.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::remove_dir_all(&app);
            // ditto keeps the bundle's metadata and code signature intact.
            let out = std::process::Command::new("ditto").arg(&bundle).arg(&app).output()?;
            if !out.status.success() {
                anyhow::bail!("ditto {}: {}", app.display(), String::from_utf8_lossy(&out.stderr).trim());
            }
            println!("control center: {}", app.display());
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let share = paths::home_dir().join(".local/share");
        let icon_dir = share.join("icons/hicolor/256x256/apps");
        std::fs::create_dir_all(&icon_dir)?;
        std::fs::write(icon_dir.join("git-drive-control-center.png"), CONTROL_CENTER_ICON)?;
        let apps = share.join("applications");
        std::fs::create_dir_all(&apps)?;
        std::fs::write(apps.join("git-drive-control-center.desktop"), render_desktop_entry(&target))?;
        println!("control center: launcher entry in {}", apps.display());
    }
    Ok(Some(target))
}

#[cfg(not(target_os = "macos"))]
const CONTROL_CENTER_ICON: &[u8] = include_bytes!("../control-center/src-tauri/icons/128x128@2x.png");

/// Launcher entry for the Control Center (pure). `Exec` quotes the path with
/// the Desktop Entry escapes: `"`, `` ` ``, `$` and `\` inside quotes, `%`
/// doubled.
pub fn render_desktop_entry(binary: &Path) -> String {
    let mut exec = String::from("\"");
    for ch in binary.to_string_lossy().chars() {
        match ch {
            '"' | '`' | '$' => {
                exec.push('\\');
                exec.push(ch);
            }
            // A literal backslash in a quoted argument takes four (string
            // escape first, then the quoting rule).
            '\\' => exec.push_str("\\\\\\\\"),
            '%' => exec.push_str("%%"),
            _ => exec.push(ch),
        }
    }
    exec.push('"');
    format!(
        "[Desktop Entry]\nType=Application\nName=git-drive Control Center\nComment=Status, logs and settings of git-drive\nExec={}\nIcon=git-drive-control-center\nTerminal=false\nCategories=Utility;System;\nStartupWMClass=git-drive-control-center\n",
        exec
    )
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
    fn the_desktop_entry_launches_the_installed_control_center() {
        let entry = render_desktop_entry(Path::new("/home/u/my apps/git-drive-control-center"));
        assert!(entry.starts_with("[Desktop Entry]\n"), "{}", entry);
        assert!(entry.contains("\nExec=\"/home/u/my apps/git-drive-control-center\"\n"), "{}", entry);
        assert!(entry.contains("\nIcon=git-drive-control-center\n"), "{}", entry);
        assert!(entry.contains("\nType=Application\n"), "{}", entry);
        let tricky = render_desktop_entry(Path::new("/x/a\"b$c/bin"));
        assert!(tricky.contains("Exec=\"/x/a\\\"b\\$c/bin\""), "{}", tricky);
    }

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
