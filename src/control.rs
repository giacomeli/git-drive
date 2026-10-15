//! Start, stop and restart the daemon through its login service (LaunchAgent
//! on macOS, systemd user unit on Linux). Never by signalling the process:
//! the service stop is the clean one (SIGTERM -> unmount -> final drain).

use std::path::Path;

use serde::Serialize;

use crate::status::SyncStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    Stopped,
    NotInstalled,
}

fn cmd(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

/// launchctl commands for an action (pure). Start enables the label first:
/// a `launchctl disable` left from a manual stop would make the bootstrap fail.
pub fn launchctl_plan(action: Action, domain: &str, plist: &Path) -> Vec<Vec<String>> {
    let target = format!("{}/{}", domain, crate::launchd::LABEL);
    let plist = plist.to_string_lossy();
    let start = vec![
        cmd(&["launchctl", "enable", &target]),
        cmd(&["launchctl", "bootstrap", domain, &plist]),
    ];
    let stop = vec![cmd(&["launchctl", "bootout", &target])];
    match action {
        Action::Start => start,
        Action::Stop => stop,
        Action::Restart => stop.into_iter().chain(start).collect(),
    }
}

/// systemctl commands for an action (pure).
pub fn systemctl_plan(action: Action) -> Vec<Vec<String>> {
    let verb = match action {
        Action::Start => "start",
        Action::Stop => "stop",
        Action::Restart => "restart",
    };
    vec![cmd(&["systemctl", "--user", verb, crate::systemd::UNIT])]
}

/// Whether the login service is installed on this machine.
pub fn installed() -> bool {
    if cfg!(target_os = "macos") {
        crate::launchd::agent_plist_path().exists()
    } else {
        crate::systemd::user_unit_dir().join(crate::systemd::UNIT).exists()
    }
}

/// State from the facts (pure): installed service plus a live daemon.
pub fn state_of(installed: bool, status: Option<&SyncStatus>) -> ServiceState {
    if !installed {
        ServiceState::NotInstalled
    } else if status.is_some_and(crate::status::daemon_running) {
        ServiceState::Running
    } else {
        ServiceState::Stopped
    }
}

/// Command that opens the Control Center (pure): the macOS app bundle when
/// installed (`open -n` starts a second instance, which hands its arguments
/// to the open window and exits), otherwise the installed binary.
pub fn control_center_argv(app: Option<&Path>, binary: Option<&Path>, page: Option<&str>) -> Option<Vec<String>> {
    let page_args = page.map(|p| vec!["--page".to_string(), p.to_string()]).unwrap_or_default();
    if let Some(app) = app {
        let mut argv = cmd(&["open", "-n", "-a", &app.to_string_lossy()]);
        if !page_args.is_empty() {
            argv.push("--args".into());
            argv.extend(page_args);
        }
        return Some(argv);
    }
    let mut argv = vec![binary?.to_string_lossy().into_owned()];
    argv.extend(page_args);
    Some(argv)
}

/// Opens the Control Center at `page`. `Ok(false)` = not installed (the
/// caller falls back to the status viewer).
pub fn open_control_center(page: Option<&str>) -> anyhow::Result<bool> {
    let app = crate::paths::control_center_app_path();
    let app = (cfg!(target_os = "macos") && app.is_dir()).then_some(app);
    // The installed binary, or one built next to this executable (dev tree).
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("git-drive-control-center")));
    let binary = [Some(crate::paths::installed_control_center_path()), sibling]
        .into_iter()
        .flatten()
        .find(|p| p.is_file());
    let Some(argv) = control_center_argv(app.as_deref(), binary.as_deref(), page) else {
        return Ok(false);
    };
    let child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(e) = &child {
        anyhow::bail!("{}: {}", argv[0], e);
    }
    crate::notify::reap_in_background(child);
    Ok(true)
}

/// Runs `action` and returns once the service manager accepted it. On macOS
/// a stop waits for the daemon to leave (its unmount and final drain take
/// seconds), so a restart never bootstraps over a daemon still stopping.
pub fn run(action: Action) -> anyhow::Result<()> {
    if !installed() {
        anyhow::bail!("the git-drive service is not installed — run `git-drive install -r <repo> -m <mount>`");
    }
    if cfg!(target_os = "macos") {
        if matches!(action, Action::Stop | Action::Restart) {
            crate::launchd::bootout_and_wait(crate::launchd::LABEL);
        }
        if matches!(action, Action::Start | Action::Restart) {
            let domain = format!("gui/{}", unsafe { libc::getuid() });
            for argv in launchctl_plan(Action::Start, &domain, &crate::launchd::agent_plist_path()) {
                let out = std::process::Command::new(&argv[0]).args(&argv[1..]).output()?;
                // Bootstrapping a service that is already loaded is not an error here.
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !out.status.success() && argv[1] == "bootstrap" && !stderr.contains("already") {
                    anyhow::bail!("{} failed: {}", argv.join(" "), stderr.trim());
                }
            }
        }
        return Ok(());
    }
    for argv in systemctl_plan(action) {
        let out = std::process::Command::new(&argv[0]).args(&argv[1..]).output()?;
        if !out.status.success() {
            anyhow::bail!("{} failed: {}", argv.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn argv(plan: &[Vec<String>]) -> Vec<String> {
        plan.iter().map(|c| c.join(" ")).collect()
    }

    #[test]
    fn launchd_plans_use_the_clean_stop() {
        let plist = Path::new("/Users/u/Library/LaunchAgents/dev.giacomeli.git-drive.plist");
        let start = launchctl_plan(Action::Start, "gui/501", plist);
        assert_eq!(
            argv(&start),
            vec![
                "launchctl enable gui/501/dev.giacomeli.git-drive",
                "launchctl bootstrap gui/501 /Users/u/Library/LaunchAgents/dev.giacomeli.git-drive.plist",
            ]
        );
        assert_eq!(argv(&launchctl_plan(Action::Stop, "gui/501", plist)), vec!["launchctl bootout gui/501/dev.giacomeli.git-drive"]);
        let restart = argv(&launchctl_plan(Action::Restart, "gui/501", plist));
        assert_eq!(restart.len(), 3, "bootout, then enable + bootstrap: {:?}", restart);
        assert!(restart[0].contains("bootout") && restart[2].contains("bootstrap"));
    }

    #[test]
    fn systemd_plans_drive_the_user_unit() {
        assert_eq!(argv(&systemctl_plan(Action::Start)), vec!["systemctl --user start git-drive.service"]);
        assert_eq!(argv(&systemctl_plan(Action::Stop)), vec!["systemctl --user stop git-drive.service"]);
        assert_eq!(argv(&systemctl_plan(Action::Restart)), vec!["systemctl --user restart git-drive.service"]);
    }

    #[test]
    fn the_control_center_opens_the_app_bundle_or_the_installed_binary() {
        let app = Path::new("/Users/u/Applications/git-drive Control Center.app");
        let bin = Path::new("/home/u/.local/bin/git-drive-control-center");
        assert_eq!(
            control_center_argv(Some(app), Some(bin), Some("logs")).unwrap(),
            vec!["open", "-n", "-a", "/Users/u/Applications/git-drive Control Center.app", "--args", "--page", "logs"],
            "the bundle wins: it has the Dock icon and the app name"
        );
        assert_eq!(control_center_argv(None, Some(bin), None).unwrap(), vec!["/home/u/.local/bin/git-drive-control-center"]);
        assert_eq!(
            control_center_argv(None, Some(bin), Some("logs")).unwrap(),
            vec!["/home/u/.local/bin/git-drive-control-center", "--page", "logs"]
        );
        assert!(control_center_argv(None, None, Some("logs")).is_none());
    }

    #[test]
    fn state_needs_the_service_and_a_running_daemon() {
        let mut s = crate::status::SyncStatus { pid: std::process::id(), ..Default::default() };
        assert_eq!(state_of(false, Some(&s)), ServiceState::NotInstalled);
        assert_eq!(state_of(true, Some(&s)), ServiceState::Running);
        s.stopped_at = Some(1);
        assert_eq!(state_of(true, Some(&s)), ServiceState::Stopped);
        assert_eq!(state_of(true, None), ServiceState::Stopped);
    }
}
