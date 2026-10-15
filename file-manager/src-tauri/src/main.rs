//! git-drive Files (proof of concept): a read-only file manager with the
//! Control Center's look. The filesystem logic lives in `fsmodel`; this file
//! adapts it to Tauri commands and builds the window.

mod fsmodel;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fsmodel::{Entry, Listing, Place, PreviewKind, SearchOptions, TextPreview};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{State, WebviewUrl, WebviewWindowBuilder};

/// Text previews stop here; the rest of a long file opens in its app.
const TEXT_PREVIEW_BYTES: usize = 256 * 1024;
/// Search results reach the window in batches of this size, or this often.
const SEARCH_BATCH: usize = 50;
const SEARCH_FLUSH: Duration = Duration::from_millis(150);

struct AppState {
    /// Bumped by every search; a running search stops once it is no longer the latest.
    search_generation: Arc<AtomicU64>,
    /// Which native effect the window got: "liquid-glass", "vibrancy" or "none".
    glass: Mutex<&'static str>,
    start: PathBuf,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The git-drive mount point, from the daemon's status file.
fn drive_mount() -> Option<PathBuf> {
    let status = git_drive::status::read_from(&git_drive::paths::standard_status_path()).ok()?;
    let mount = PathBuf::from(status.mount);
    mount.is_dir().then_some(mount)
}

#[derive(Serialize)]
struct Boot {
    start: String,
    places: Vec<Place>,
    /// Accent color chosen in the Control Center (shared preference).
    accent: String,
    glass: &'static str,
    platform: &'static str,
}

#[tauri::command]
fn boot(state: State<'_, AppState>) -> Boot {
    let prefs = git_drive::paths::config_dir().join("control-center.json");
    let accent = std::fs::read_to_string(prefs)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("accent").and_then(|a| a.as_str()).map(str::to_string))
        .unwrap_or_else(|| "multicolor".into());
    Boot {
        start: state.start.display().to_string(),
        places: fsmodel::places(&home(), drive_mount().as_deref()),
        accent,
        glass: *state.glass.lock().unwrap(),
        platform: if cfg!(target_os = "macos") { "macOS" } else { "Linux" },
    }
}

#[tauri::command]
async fn list_dir(path: String, show_hidden: bool) -> Result<Listing, String> {
    tauri::async_runtime::spawn_blocking(move || fsmodel::list_dir(Path::new(&path), show_hidden))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn preview_kind(path: String) -> PreviewKind {
    fsmodel::classify(Path::new(&path))
}

#[tauri::command]
async fn read_text(path: String) -> Result<TextPreview, String> {
    tauri::async_runtime::spawn_blocking(move || fsmodel::read_text(Path::new(&path), TEXT_PREVIEW_BYTES))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct SearchSummary {
    truncated: bool,
    cancelled: bool,
    visited: usize,
}

/// Name search under `root`. Matches stream through `found` in batches (a
/// search over the whole drive takes seconds); the reply only closes the
/// search. A newer call cancels the running one, which replies `cancelled`.
#[tauri::command]
async fn search(
    state: State<'_, AppState>,
    root: String,
    query: String,
    show_hidden: bool,
    found: Channel<Vec<Entry>>,
) -> Result<SearchSummary, String> {
    let generation = state.search_generation.clone();
    let mine = generation.fetch_add(1, Ordering::SeqCst) + 1;
    tauri::async_runtime::spawn_blocking(move || {
        let opts = SearchOptions { show_hidden, ..SearchOptions::default() };
        let mut batch: Vec<Entry> = Vec::new();
        let mut last_flush = Instant::now();
        let result = fsmodel::search(
            Path::new(&root),
            &query,
            &opts,
            &|| generation.load(Ordering::SeqCst) != mine,
            &mut |entry| {
                batch.push(entry.clone());
                if batch.len() >= SEARCH_BATCH || last_flush.elapsed() >= SEARCH_FLUSH {
                    let _ = found.send(std::mem::take(&mut batch));
                    last_flush = Instant::now();
                }
            },
        );
        if !batch.is_empty() {
            let _ = found.send(batch);
        }
        SearchSummary { truncated: result.truncated, cancelled: result.cancelled, visited: result.visited }
    })
    .await
    .map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct Space {
    total: u64,
    free: u64,
}

/// Free space where `path` lives. Inside the git-drive mount the FUSE volume
/// reports placeholder numbers, so the repository's real volume is measured.
#[tauri::command]
fn volume(path: String) -> Option<Space> {
    let path = PathBuf::from(path);
    let status = git_drive::status::read_from(&git_drive::paths::standard_status_path()).ok();
    let measured = match status {
        Some(s) if !s.mount.is_empty() && path.starts_with(&s.mount) => PathBuf::from(s.repo),
        _ => path,
    };
    git_drive::storage::volume(&measured).ok().map(|v| Space { total: v.total, free: v.free })
}

/// Opens a file in its default app, or reveals it in the file manager.
#[tauri::command]
fn open_path(path: String, reveal: bool) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let child = if cfg!(target_os = "macos") {
        let mut cmd = std::process::Command::new("open");
        if reveal {
            cmd.arg("-R");
        }
        cmd.arg(&path).spawn()
    } else {
        let target = if reveal { path.parent().unwrap_or(&path).to_path_buf() } else { path };
        std::process::Command::new("xdg-open").arg(target).spawn()
    };
    child.as_ref().map_err(|e| e.to_string())?;
    git_drive::notify::reap_in_background(child);
    Ok(())
}

/// Native glass behind the transparent webview: Liquid Glass on macOS 26+,
/// the classic vibrancy material before it (same as the Control Center).
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

/// `--path <dir>`: the folder the window opens at.
fn path_arg(args: &[String]) -> Option<PathBuf> {
    let idx = args.iter().position(|a| a == "--path")?;
    let path = PathBuf::from(args.get(idx + 1)?);
    path.is_dir().then_some(path)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let start = path_arg(&args).or_else(drive_mount).unwrap_or_else(home);
    let state = AppState { search_generation: Arc::default(), glass: Mutex::new("none"), start };
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![boot, list_dir, preview_kind, read_text, search, volume, open_path])
        .setup(|app| {
            let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::default())
                .title("git-drive Files")
                .inner_size(1240.0, 800.0)
                .min_inner_size(900.0, 560.0)
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
        .expect("error while running git-drive Files");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_arg_accepts_only_existing_folders() {
        let dir = std::env::temp_dir();
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(path_arg(&args(&["f", "--path", dir.to_str().unwrap()])), Some(dir.clone()));
        assert_eq!(path_arg(&args(&["f", "--path", "/definitely/not/here"])), None);
        assert_eq!(path_arg(&args(&["f"])), None);
    }
}
