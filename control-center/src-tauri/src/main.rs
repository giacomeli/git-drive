//! git-drive Control Center: a window over what the daemon publishes (status
//! file, session log, config) plus control of its login service. Every piece
//! of logic lives in the `git_drive` library (`control`, `storage`,
//! `logview`, `Config::save_to`, `git::commit_activity`); this file only
//! adapts it to Tauri commands and builds the window.

mod prefs;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use git_drive::config::Config;
use git_drive::control::{self, Action, ServiceState};
use git_drive::logview::{self, LogEntry};
use git_drive::status::{self, SyncStatus};
use git_drive::{paths, storage};
use serde::Serialize;
use tauri::{State, WebviewUrl, WebviewWindowBuilder};

/// The `.git` walk is cached: the overview polls every 2 s.
const REPO_SIZE_TTL: Duration = Duration::from_secs(60);
/// Activity chart: commits per hour over the last day.
const ACTIVITY_BUCKETS: usize = 24;
const ACTIVITY_BUCKET_SECS: i64 = 3600;

#[derive(Default)]
struct RepoSize {
    repo: PathBuf,
    bytes: Option<u64>,
    computed_at: Option<Instant>,
    running: bool,
}

struct AppState {
    repo_size: Arc<Mutex<RepoSize>>,
    /// Which native effect the window got: "liquid-glass", "vibrancy" or "none".
    glass: Mutex<&'static str>,
}

#[derive(Serialize)]
struct Overview {
    now: u64,
    service: ServiceState,
    status: Option<SyncStatus>,
    volume: Option<storage::Volume>,
    repo_bytes: Option<u64>,
    activity: Vec<u32>,
}

#[derive(Serialize)]
struct LogTail {
    file: String,
    entries: Vec<LogEntry>,
}

#[derive(Serialize)]
struct ConfigView {
    path: String,
    exists: bool,
    config: Config,
}

#[derive(Serialize)]
struct PathInfo {
    label: &'static str,
    path: String,
    exists: bool,
}

#[derive(Serialize)]
struct About {
    version: &'static str,
    platform: &'static str,
    glass: &'static str,
    service_installed: bool,
    service_name: &'static str,
    git_transport: String,
    encryption: bool,
    p2p: bool,
    paths: Vec<PathInfo>,
}

fn read_status() -> Option<SyncStatus> {
    status::read_from(&paths::standard_status_path()).ok()
}

/// Config file the installed service uses (`install` points it there),
/// never a `./config.json` from the current directory.
fn config_path() -> PathBuf {
    paths::standard_config_path()
}

fn load_config() -> Config {
    Config::load_from(&config_path().to_string_lossy())
}

/// Size of the repository's `.git`, recomputed in the background when stale.
fn repo_bytes(cache: &Arc<Mutex<RepoSize>>, repo: &Path) -> Option<u64> {
    let mut guard = cache.lock().unwrap();
    if guard.repo != repo {
        *guard = RepoSize { repo: repo.to_path_buf(), ..Default::default() };
    }
    let stale = guard.computed_at.is_none_or(|at| at.elapsed() > REPO_SIZE_TTL);
    if stale && !guard.running {
        guard.running = true;
        let cache = cache.clone();
        let git_dir = repo.join(".git");
        let repo = repo.to_path_buf();
        std::thread::spawn(move || {
            let bytes = storage::dir_size(&git_dir);
            let mut guard = cache.lock().unwrap();
            if guard.repo == repo {
                guard.bytes = Some(bytes);
                guard.computed_at = Some(Instant::now());
            }
            guard.running = false;
        });
    }
    guard.bytes
}

#[tauri::command]
fn overview(state: State<'_, AppState>) -> Overview {
    let now = status::now_unix();
    let status = read_status();
    let service = control::state_of(control::installed(), status.as_ref());
    let repo = status.as_ref().map(|s| PathBuf::from(&s.repo)).filter(|p| p.is_dir());
    let volume = repo.as_deref().and_then(|r| storage::volume(r).ok());
    let repo_bytes = repo.as_deref().and_then(|r| repo_bytes(&state.repo_size, r));
    let activity = repo
        .as_deref()
        .and_then(|r| git2::Repository::open(r).ok())
        .and_then(|r| git_drive::git::commit_activity(&r, now as i64, ACTIVITY_BUCKETS, ACTIVITY_BUCKET_SECS).ok())
        .unwrap_or_default();
    Overview { now, service, status, volume, repo_bytes, activity }
}

/// The session log: the file the status names, or the current file of the
/// standard log directory when the daemon never wrote a status.
fn log_file() -> Option<PathBuf> {
    let named = read_status().map(|s| PathBuf::from(s.log_file)).filter(|p| p.is_file());
    named.or_else(|| {
        let current = paths::standard_log_dir().join("git-drive_rCURRENT.log");
        current.is_file().then_some(current)
    })
}

#[tauri::command]
fn logs(max: usize) -> Result<LogTail, String> {
    let Some(file) = log_file() else {
        return Ok(LogTail { file: String::new(), entries: Vec::new() });
    };
    let entries = logview::tail(&file, max.clamp(1, 5000)).map_err(|e| e.to_string())?;
    Ok(LogTail { file: file.display().to_string(), entries })
}

#[tauri::command]
fn get_config() -> Result<ConfigView, String> {
    let path = config_path();
    let exists = path.exists();
    let config = if exists {
        Config::try_load_from(&path.to_string_lossy())?
    } else {
        Config::default()
    };
    Ok(ConfigView { path: path.display().to_string(), exists, config })
}

#[tauri::command]
fn save_config(config: Config) -> Result<(), String> {
    config.save_to(&config_path()).map_err(|e| e.to_string())
}

/// Start/stop/restart through the login service. Runs off the main thread:
/// a stop waits for the daemon's unmount and final drain.
#[tauri::command]
async fn daemon(action: String) -> Result<(), String> {
    let action = match action.as_str() {
        "start" => Action::Start,
        "stop" => Action::Stop,
        "restart" => Action::Restart,
        other => return Err(format!("unknown action {:?}", other)),
    };
    tauri::async_runtime::spawn_blocking(move || control::run(action))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_ui_prefs() -> prefs::UiPrefs {
    prefs::load(&prefs::path())
}

#[tauri::command]
fn save_ui_prefs(prefs: prefs::UiPrefs) -> Result<(), String> {
    prefs::save(&prefs::path(), &prefs).map_err(|e| e.to_string())
}

/// Opens a folder (or reveals a file) in the file manager.
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let child = if cfg!(target_os = "macos") {
        let mut cmd = std::process::Command::new("open");
        if path.is_file() {
            cmd.arg("-R");
        }
        cmd.arg(&path).spawn()
    } else {
        let target = if path.is_file() { path.parent().unwrap_or(&path).to_path_buf() } else { path };
        std::process::Command::new("xdg-open").arg(target).spawn()
    };
    child.as_ref().map_err(|e| e.to_string())?;
    git_drive::notify::reap_in_background(child);
    Ok(())
}

#[tauri::command]
fn about(state: State<'_, AppState>) -> About {
    let config = load_config();
    let config_file = config_path();
    let service_file = if cfg!(target_os = "macos") {
        git_drive::launchd::agent_plist_path()
    } else {
        git_drive::systemd::user_unit_dir().join(git_drive::systemd::UNIT)
    };
    let device_key = config
        .p2p_key_file
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| config_file.parent().unwrap_or(Path::new(".")).join("peer.key"));
    let entry = |label: &'static str, path: PathBuf| PathInfo {
        label,
        exists: path.exists(),
        path: path.display().to_string(),
    };
    let mut list = vec![
        entry("Config", config_file.clone()),
        entry("Status", paths::standard_status_path()),
        entry("Logs", paths::standard_log_dir()),
        entry("Daemon binary", paths::installed_binary_path()),
        entry("Login service", service_file),
        entry("Window preferences", prefs::path()),
    ];
    if config.encryption_enabled {
        list.push(entry("Encryption key", paths::resolve_key_for_read(config.encryption_key_file.as_deref())));
    }
    if config.p2p_enabled {
        list.push(entry("Device key", device_key));
    }
    if let Some(s) = read_status() {
        list.insert(0, entry("Repository", PathBuf::from(&s.repo)));
        list.insert(1, entry("Drive", PathBuf::from(&s.mount)));
    }
    About {
        version: env!("CARGO_PKG_VERSION"),
        platform: if cfg!(target_os = "macos") { "macOS" } else { "Linux" },
        glass: *state.glass.lock().unwrap(),
        service_installed: control::installed(),
        service_name: if cfg!(target_os = "macos") { git_drive::launchd::LABEL } else { git_drive::systemd::UNIT },
        git_transport: config.git_transport.clone(),
        encryption: config.encryption_enabled,
        p2p: config.p2p_enabled,
        paths: list,
    }
}

/// Native glass behind the transparent webview: Liquid Glass on macOS 26+,
/// the classic vibrancy material before it.
#[cfg(target_os = "macos")]
fn apply_glass(window: &tauri::WebviewWindow) -> &'static str {
    use window_vibrancy::{
        apply_liquid_glass, apply_vibrancy, LiquidGlassOptions, NSGlassEffectViewStyle, NSVisualEffectMaterial,
        NSVisualEffectState,
    };
    let glass = LiquidGlassOptions::new(NSGlassEffectViewStyle::Regular).tint_color((28, 26, 58, 150));
    if apply_liquid_glass(window, glass).is_ok() {
        return "liquid-glass";
    }
    match apply_vibrancy(window, NSVisualEffectMaterial::HudWindow, Some(NSVisualEffectState::Active), None) {
        Ok(()) => "vibrancy",
        Err(_) => "none",
    }
}

const PAGES: [&str; 4] = ["overview", "logs", "settings", "about"];

/// `--page <name>`: the page the window opens at (the tray's "Show log" opens `logs`).
fn page_arg(args: &[String]) -> Option<&'static str> {
    let idx = args.iter().position(|a| a == "--page")?;
    let wanted = args.get(idx + 1)?;
    PAGES.iter().copied().find(|p| p == wanted)
}

fn main() {
    let state = AppState { repo_size: Arc::default(), glass: Mutex::new("none") };
    let args: Vec<String> = std::env::args().collect();
    let start_page = page_arg(&args).unwrap_or("overview");
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            use tauri::{Emitter, Manager};
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
                if let Some(page) = page_arg(&argv) {
                    let _ = window.emit("navigate", page);
                }
            }
        }))
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            overview,
            logs,
            get_config,
            save_config,
            daemon,
            open_path,
            about,
            get_ui_prefs,
            save_ui_prefs
        ])
        .setup(move |app| {
            let url = WebviewUrl::App(format!("index.html#{}", start_page).into());
            let builder = WebviewWindowBuilder::new(app, "main", url)
                .title("git-drive")
                .inner_size(1180.0, 780.0)
                .min_inner_size(960.0, 640.0)
                .transparent(true);
            #[cfg(target_os = "macos")]
            let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay).hidden_title(true);
            #[cfg(not(target_os = "macos"))]
            let builder = builder.decorations(false);
            #[allow(unused_variables)]
            let window = builder.build()?;
            #[cfg(target_os = "macos")]
            {
                use tauri::Manager;
                *app.state::<AppState>().glass.lock().unwrap() = apply_glass(&window);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Control Center");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_arg_accepts_only_known_pages() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(page_arg(&args(&["cc", "--page", "logs"])), Some("logs"));
        assert_eq!(page_arg(&args(&["cc", "--page", "nope"])), None);
        assert_eq!(page_arg(&args(&["cc", "--page"])), None);
        assert_eq!(page_arg(&args(&["cc"])), None);
    }
}
