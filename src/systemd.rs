//! Linux login service: systemd user units for the daemon and the tray.
//!
//! Same shape as the LaunchAgent (`launchd.rs`): units rendered from string
//! templates (pure, testable), every path passed as an explicit flag so the
//! unit is the readable source of truth for where the daemon reads and
//! writes. `Restart=on-failure` mirrors KeepAlive `{SuccessfulExit=false}`:
//! a crash restarts, a clean stop (SIGTERM/unmount, exit 0) does not.

use std::path::{Path, PathBuf};
#[cfg(not(target_os = "macos"))]
use std::process::Command;

#[cfg(not(target_os = "macos"))]
use crate::{paths, service};

pub const UNIT: &str = "git-drive.service";
pub const TRAY_UNIT: &str = "git-drive-tray.service";

/// Hand-written units from the pre-rename era (project was drive-sync).
#[cfg(not(target_os = "macos"))]
const LEGACY_UNITS: [&str; 2] = ["drive-sync.service", "drive-sync-tray.service"];

/// Quotes one ExecStart argument: double quotes, with `\` and `"` escaped,
/// `%` doubled (systemd specifiers) and `$` doubled (env expansion).
fn quote_arg(arg: &str) -> String {
    let escaped = arg
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
        .replace('$', "$$");
    format!("\"{}\"", escaped)
}

fn exec_line(binary: &Path, args: &[&str]) -> String {
    let mut parts = vec![quote_arg(&binary.to_string_lossy())];
    parts.extend(args.iter().map(|a| quote_arg(a)));
    parts.join(" ")
}

/// Renders the daemon unit. Pure and testable.
pub fn render_unit(
    binary: &Path,
    repo: &Path,
    mount: &Path,
    config: &Path,
    log_dir: &Path,
    status_file: &Path,
) -> String {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let (repo, mount, config, log_dir, status) =
        (s(repo), s(mount), s(config), s(log_dir), s(status_file));
    let exec = exec_line(
        binary,
        &[
            "-r",
            &repo,
            "-m",
            &mount,
            "--config",
            &config,
            "--log-dir",
            &log_dir,
            "--status-file",
            &status,
        ],
    );
    format!(
        "[Unit]
Description=git-drive: Git repository mounted as a drive
After=network-online.target

[Service]
Type=simple
ExecStart={exec}
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
# Headroom for the final drain (unmount -> commit -> push) before SIGKILL.
TimeoutStopSec=90

[Install]
WantedBy=default.target
"
    )
}

/// Renders the tray unit (bound to the graphical session). Pure and testable.
pub fn render_tray_unit(binary: &Path, status_file: &Path) -> String {
    let status = status_file.to_string_lossy().into_owned();
    let exec = exec_line(binary, &["tray", "--file", &status]);
    format!(
        "[Unit]
Description=git-drive: status icon in the tray
PartOf=graphical-session.target
After=graphical-session.target

[Service]
Type=simple
ExecStart={exec}
Restart=on-failure
RestartSec=10

[Install]
WantedBy=graphical-session.target
"
    )
}

/// `~/.config/systemd/user` (honors an absolute `$XDG_CONFIG_HOME`).
pub fn user_unit_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("systemd/user")
}

#[cfg(not(target_os = "macos"))]
fn systemctl(args: &[&str]) -> anyhow::Result<std::process::Output> {
    Ok(Command::new("systemctl").arg("--user").args(args).output()?)
}

/// Installs and starts the units. Legacy `drive-sync*` units are stopped,
/// disabled and kept as `<unit>.bak` (they were hand-written — never deleted).
#[cfg(not(target_os = "macos"))]
pub fn install(repo: &str, mount: &str, with_tray: bool) -> anyhow::Result<()> {
    let binary = service::install_binary()?;
    let repo = std::fs::canonicalize(repo)
        .map_err(|e| anyhow::anyhow!("repo {} inaccessible: {}", repo, e))?;
    std::fs::create_dir_all(mount)?;
    let mount = std::fs::canonicalize(mount)?;

    let log_dir = paths::standard_log_dir();
    std::fs::create_dir_all(&log_dir)?;
    service::migrate_legacy_config();
    let config = service::ensure_config()?;
    let status_file = paths::standard_status_path();

    let unit_dir = user_unit_dir();
    std::fs::create_dir_all(&unit_dir)?;

    for legacy in LEGACY_UNITS {
        let path = unit_dir.join(legacy);
        if path.exists() {
            let _ = systemctl(&["disable", "--now", legacy]);
            let bak = unit_dir.join(format!("{legacy}.bak"));
            match std::fs::rename(&path, &bak) {
                Ok(_) => println!("legacy unit {} stopped, kept as {}", legacy, bak.display()),
                Err(e) => eprintln!("warning: could not move {}: {}", path.display(), e),
            }
        }
    }

    let unit_path = unit_dir.join(UNIT);
    std::fs::write(
        &unit_path,
        render_unit(&binary, &repo, &mount, &config, &log_dir, &status_file),
    )?;
    if with_tray {
        std::fs::write(unit_dir.join(TRAY_UNIT), render_tray_unit(&binary, &status_file))?;
    }
    systemctl(&["daemon-reload"])?;

    // Re-install: restart so the new binary/arguments take effect.
    let out = systemctl(&["enable", UNIT])?;
    if !out.status.success() {
        anyhow::bail!(
            "systemctl enable failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let out = systemctl(&["restart", UNIT])?;
    if !out.status.success() {
        anyhow::bail!(
            "systemctl restart failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if with_tray {
        let _ = systemctl(&["enable", TRAY_UNIT]);
        let out = systemctl(&["restart", TRAY_UNIT])?;
        if !out.status.success() {
            eprintln!(
                "warning: tray did not start ({}) — it needs a graphical session and, on GNOME, the AppIndicator extension",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }

    println!("  unit:   {}", unit_path.display());
    println!("  repo:   {}", repo.display());
    println!("  mount:  {}", mount.display());
    println!("  config: {}", config.display());
    println!("  logs:   {}", log_dir.display());
    println!("  status: {} (git-drive status finds it on its own)", status_file.display());
    if !linger_enabled() {
        println!(
            "note: the drive only runs while you are logged in. To keep it running after logout and start it at boot: loginctl enable-linger $USER"
        );
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn linger_enabled() -> bool {
    let user = std::env::var("USER").unwrap_or_default();
    Command::new("loginctl")
        .args(["show-user", &user, "-p", "Linger", "--value"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes")
        .unwrap_or(false)
}

/// Stops and removes the units (the clean stop drains the queue first).
#[cfg(not(target_os = "macos"))]
pub fn uninstall() -> anyhow::Result<()> {
    let unit_dir = user_unit_dir();
    for unit in [TRAY_UNIT, UNIT] {
        let _ = systemctl(&["disable", "--now", unit]);
        match std::fs::remove_file(unit_dir.join(unit)) {
            Ok(()) => println!("removed {}", unit),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_unit_quotes_paths_and_keeps_argument_order() {
        let unit = render_unit(
            Path::new("/home/u/.local/bin/git-drive"),
            Path::new("/home/u/my repo"),
            Path::new("/home/u/GitDrive"),
            Path::new("/home/u/.config/git-drive/config.json"),
            Path::new("/home/u/.local/state/git-drive/logs"),
            Path::new("/home/u/.local/state/git-drive/status.json"),
        );
        assert!(unit.contains(
            "ExecStart=\"/home/u/.local/bin/git-drive\" \"-r\" \"/home/u/my repo\" \"-m\" \"/home/u/GitDrive\""
        ));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target"));
        assert!(unit.contains("TimeoutStopSec="));
    }

    #[test]
    fn quote_arg_escapes_systemd_specials() {
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("100%"), "\"100%%\"");
        assert_eq!(quote_arg("$HOME"), "\"$$HOME\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(quote_arg("back\\slash"), "\"back\\\\slash\"");
    }

    #[test]
    fn render_tray_unit_binds_to_graphical_session() {
        let unit = render_tray_unit(
            Path::new("/bin/git-drive"),
            Path::new("/s/status.json"),
        );
        assert!(unit.contains("PartOf=graphical-session.target"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
        assert!(unit.contains("ExecStart=\"/bin/git-drive\" \"tray\" \"--file\" \"/s/status.json\""));
    }
}
