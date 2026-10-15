//! Generation and management of the macOS LaunchAgent.
//!
//! The plist is generated from a string template (decision 4 of Phase 2):
//! small, fixed structure, auditable in diffs and tests. The config, log and
//! status paths go as explicit flags in ProgramArguments — the plist is the
//! readable source of truth for where the daemon reads and writes (decision 10).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::paths;

pub const LABEL: &str = "dev.giacomeli.git-drive";
pub const TRAY_LABEL: &str = "dev.giacomeli.git-drive.tray";

/// Pre-rename labels (project was called drive-sync until 2026-07). Agents
/// registered under these are taken down and their plists removed on install.
const LEGACY_LABELS: [&str; 2] = ["dev.giacomeli.drive-sync", "dev.giacomeli.drive-sync.tray"];

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn launch_agents_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("Library/LaunchAgents")
}

/// Path of the agent plist: `~/Library/LaunchAgents/<label>.plist`.
pub fn agent_plist_path() -> PathBuf {
    launch_agents_dir().join(format!("{}.plist", LABEL))
}

pub fn tray_plist_path() -> PathBuf {
    launch_agents_dir().join(format!("{}.plist", TRAY_LABEL))
}

/// Plist for the tray agent (starts at login; no KeepAlive — closing via the
/// "Quit tray" option must not resurrect it).
pub fn render_tray_plist(binary: &Path, status_file: &Path) -> String {
    let e = |p: &Path| xml_escape(&p.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>tray</string>
        <string>--file</string>
        <string>{status}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        label = TRAY_LABEL,
        bin = e(binary),
        status = e(status_file),
    )
}

/// Renders the LaunchAgent plist. Pure and testable.
///
/// - `RunAtLoad`: mounts at login.
/// - `KeepAlive {SuccessfulExit=false}`: crash restarts; clean stop
///   (SIGTERM/eject, exit 0) does NOT resurrect (decision 5).
/// - `ExitTimeOut 60`: headroom for the final drain before launchd's SIGKILL.
pub fn render_plist(
    binary: &Path,
    repo: &Path,
    mount: &Path,
    config: &Path,
    log_dir: &Path,
    status_file: &Path,
) -> String {
    let e = |p: &Path| xml_escape(&p.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>-r</string>
        <string>{repo}</string>
        <string>-m</string>
        <string>{mount}</string>
        <string>--config</string>
        <string>{config}</string>
        <string>--log-dir</string>
        <string>{log_dir}</string>
        <string>--status-file</string>
        <string>{status}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ExitTimeOut</key>
    <integer>60</integer>
    <key>StandardOutPath</key>
    <string>{log_dir}/launchd-stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/launchd-stderr.log</string>
</dict>
</plist>
"#,
        label = LABEL,
        bin = e(binary),
        repo = e(repo),
        mount = e(mount),
        config = e(config),
        log_dir = e(log_dir),
        status = e(status_file),
    )
}

fn launchctl(args: &[&str]) -> anyhow::Result<std::process::Output> {
    Ok(Command::new("launchctl").args(args).output()?)
}

fn gui_domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// `launchctl bootout` and WAIT until the label has left the domain.
/// bootout returns while the old daemon is still stopping (unmount + final
/// drain can take seconds, up to ExitTimeOut); a bootstrap issued in that
/// window fails with "Bootstrap failed: 5: Input/output error" and leaves
/// the drive with no agent at all (seen re-installing over a live daemon).
pub(crate) fn bootout_and_wait(label: &str) {
    let target = format!("{}/{}", gui_domain(), label);
    let _ = launchctl(&["bootout", &target]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while std::time::Instant::now() < deadline {
        let still_there = launchctl(&["print", &target])
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !still_there {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    log::warn!("install: {} still registered after 90 s — bootstrapping anyway", label);
}

/// Installs the LaunchAgent: resolves absolute paths, prepares standard
/// directories, copies the config if needed, writes the plist and registers
/// it with launchd. `with_tray` registers a second agent that starts the tray
/// at login.
pub fn install(repo: &str, mount: &str, with_tray: bool) -> anyhow::Result<()> {
    let binary = crate::service::install_binary()?;
    let repo = std::fs::canonicalize(repo)
        .map_err(|e| anyhow::anyhow!("repo {} inaccessible: {}", repo, e))?;
    std::fs::create_dir_all(mount)?;
    let mount = std::fs::canonicalize(mount)?;

    let app_support = paths::app_support_dir();
    let log_dir = paths::standard_log_dir();
    std::fs::create_dir_all(&app_support)?;
    std::fs::create_dir_all(&log_dir)?;

    crate::service::migrate_legacy_config();
    let config = crate::service::ensure_config()?;

    let status_file = paths::standard_status_path();
    let plist = render_plist(&binary, &repo, &mount, &config, &log_dir, &status_file);
    let plist_path = agent_plist_path();
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist_path, plist)?;

    // Pre-rename agents: take down and remove so they never race the new ones.
    for legacy in LEGACY_LABELS {
        bootout_and_wait(legacy);
        let _ = std::fs::remove_file(launch_agents_dir().join(format!("{legacy}.plist")));
    }

    // Idempotent re-install: takes down the previous instance (if any) before
    // registering again. bootout of a nonexistent agent fails — ignored.
    bootout_and_wait(LABEL);
    let out = launchctl(&["bootstrap", &gui_domain(), plist_path.to_str().unwrap()])?;
    if !out.status.success() {
        anyhow::bail!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    if with_tray {
        let tray_plist = render_tray_plist(&binary, &status_file);
        let tray_path = tray_plist_path();
        std::fs::write(&tray_path, tray_plist)?;
        bootout_and_wait(TRAY_LABEL);
        let out = launchctl(&["bootstrap", &gui_domain(), tray_path.to_str().unwrap()])?;
        if !out.status.success() {
            log::error!(
                "install: tray bootstrap failed ({}) — run `git-drive tray` manually",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        } else {
            log::info!("install: tray registered ({})", TRAY_LABEL);
        }
    }

    log::info!("install: LaunchAgent registered ({})", LABEL);
    println!("  plist:  {}", plist_path.display());
    println!("  repo:   {}", repo.display());
    println!("  mount:  {}", mount.display());
    println!("  config: {}", config.display());
    println!("  logs:   {}", log_dir.display());
    println!("  status: {} (git-drive status finds it on its own)", status_file.display());
    Ok(())
}

/// Removes the LaunchAgent: bootout + plist removal.
pub fn uninstall() -> anyhow::Result<()> {
    let out = launchctl(&["bootout", &format!("{}/{}", gui_domain(), LABEL)])?;
    if out.status.success() {
        log::info!("uninstall: agent stopped and unregistered");
    } else {
        log::warn!(
            "uninstall: agent was not registered ({})",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let plist_path = agent_plist_path();
    match std::fs::remove_file(&plist_path) {
        Ok(()) => log::info!("uninstall: {} removed", plist_path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    // Tray (if installed): bootout of a missing agent is ignored.
    let _ = launchctl(&["bootout", &format!("{}/{}", gui_domain(), TRAY_LABEL)]);
    match std::fs::remove_file(tray_plist_path()) {
        Ok(()) => log::info!("uninstall: tray agent removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plist_contains_program_arguments_in_order() {
        let plist = render_plist(
            Path::new("/usr/local/bin/git-drive"),
            Path::new("/Users/x/repo"),
            Path::new("/Users/x/GitDrive"),
            Path::new("/Users/x/Library/Application Support/git-drive/config.json"),
            Path::new("/Users/x/Library/Logs/git-drive"),
            Path::new("/Users/x/Library/Application Support/git-drive/status.json"),
        );
        assert!(plist.contains("<string>dev.giacomeli.git-drive</string>"));
        let bin = plist.find("<string>/usr/local/bin/git-drive</string>").unwrap();
        let repo = plist.find("<string>/Users/x/repo</string>").unwrap();
        let mount = plist.find("<string>/Users/x/GitDrive</string>").unwrap();
        assert!(bin < repo && repo < mount, "argument order must be preserved");
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        assert!(plist.contains("<key>ExitTimeOut</key>"));
    }

    #[test]
    fn render_plist_escapes_xml_in_paths() {
        let plist = render_plist(
            Path::new("/bin/git-drive"),
            Path::new("/Users/x/repo & <stuff>"),
            Path::new("/mnt"),
            Path::new("/cfg.json"),
            Path::new("/logs"),
            Path::new("/status.json"),
        );
        assert!(plist.contains("/Users/x/repo &amp; &lt;stuff&gt;"));
        assert!(!plist.contains("repo & <stuff>"));
    }
}
