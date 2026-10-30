//! git-drive Files (proof of concept): a file manager with the Control
//! Center's look. The filesystem logic lives in `fsmodel` (reading) and `ops`
//! (the context menu's changes); this file adapts them to Tauri commands and
//! builds the window.

// First: its `t!` macro is used by the modules below.
#[macro_use]
mod i18n;
mod archive;
mod dnd;
mod folder_preview;
mod fsmodel;
mod native_drag;
mod openwith;
mod ops;
mod places;
mod prefs;
mod props;
mod meta;
mod tasks;
mod terminal;
mod thumbs;
mod wallpaper;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fsmodel::{Entry, Listing, PreviewKind, SearchOptions, TextPreview};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{State, WebviewUrl, WebviewWindowBuilder};

/// Search results reach the window in batches of this size, or this often.
const SEARCH_BATCH: usize = 50;
const SEARCH_FLUSH: Duration = Duration::from_millis(150);

/// When the drive's mount and the mount table were read, and what they were.
type MountsSeen = (Instant, Option<PathBuf>, Vec<places::Mount>);

struct AppState {
    /// Bumped by every search; a running search stops once it is no longer the latest.
    search_generation: Arc<AtomicU64>,
    /// Which native effect the window got: "liquid-glass", "vibrancy" or "none".
    glass: Mutex<&'static str>,
    start: PathBuf,
    /// The terminal panel's shells, by tab id.
    terminals: Mutex<HashMap<u32, Arc<terminal::Session>>>,
    next_terminal: AtomicU64,
    /// The file operations under way, by the id the window gave each.
    tasks: Arc<tasks::Tasks>,
    /// Folder preview scans under way (apart: they are no operation cards).
    previews: Arc<tasks::Tasks>,
    /// The drive's mount and the mount table, read again after a few seconds (folder previews ask often).
    mounts: Mutex<Option<MountsSeen>>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The volumes with their space; the drive's carries its repository's size
/// against what the host accepts, from the daemon's status.
fn volumes(home: &Path, drive: Option<&Path>) -> Vec<places::Volume> {
    let usage = git_drive_state::status::read_from(&git_drive_state::paths::standard_status_path()).ok().and_then(|s| s.repo_usage);
    let mut out = places::volumes(home, drive, measure);
    for v in out.iter_mut().filter(|v| v.kind == "drive") {
        v.repo = usage.clone();
    }
    out
}

/// The git-drive mount point, from the daemon's status file.
fn drive_mount() -> Option<PathBuf> {
    let status = git_drive_state::status::read_from(&git_drive_state::paths::standard_status_path()).ok()?;
    let mount = PathBuf::from(status.mount);
    mount.is_dir().then_some(mount)
}

#[derive(Serialize)]
struct Boot {
    start: String,
    /// Where the column view and the path bar start: the drive, the home
    /// folder and the mounted volumes (bookmarks are shortcuts, not anchors).
    places: Vec<places::Place>,
    /// Folders never renamed, moved or deleted from here.
    fixed: Vec<String>,
    /// Accent color, shared with the Control Center (either app can change it).
    accent: String,
    glass: &'static str,
    platform: &'static str,
    /// `bsdtar` is here: Compress and Extract Here show in the menu.
    archives: bool,
    /// The system's preferred languages ("pt-BR", "en-US" ...), first the most wanted.
    locales: Vec<String>,
    /// For About: this app's version, the processor it was built for, and Tauri's version.
    version: &'static str,
    arch: &'static str,
    tauri: &'static str,
}

#[tauri::command]
fn boot(state: State<'_, AppState>) -> Boot {
    Boot {
        start: state.start.display().to_string(),
        places: anchors(),
        fixed: protected_paths().iter().map(|p| p.display().to_string()).collect(),
        archives: archive::bsdtar().is_some(),
        accent: prefs::load_accent(&prefs::path()),
        glass: *state.glass.lock().unwrap(),
        platform: if cfg!(target_os = "macos") { "macOS" } else { "Linux" },
        locales: sys_locale::get_locales().collect(),
        version: env!("CARGO_PKG_VERSION"),
        arch: std::env::consts::ARCH,
        tauri: tauri::VERSION,
    }
}

/// The accent color, shared with the Control Center (re-read when the window
/// regains focus, so a change made there shows here).
#[tauri::command]
fn accent() -> String {
    prefs::load_accent(&prefs::path())
}

#[tauri::command]
fn save_accent(accent: String) -> Result<(), String> {
    prefs::save_accent(&prefs::path(), &accent).map_err(|e| i18n::io_text(&e))
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
async fn read_text(path: String, max: Option<u64>) -> Result<TextPreview, String> {
    let budget = fsmodel::text_budget(max);
    tauri::async_runtime::spawn_blocking(move || fsmodel::read_text(Path::new(&path), budget))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| i18n::io_text(&e))
}

/// A thumbnail for the icon view, `size` pixels at most on a side, as raw
/// bytes (PNG or JPEG; no base64 through the IPC). Empty when the file keeps
/// its icon.
#[tauri::command]
async fn thumbnail(path: String, size: u32, quicklook: Option<bool>) -> Result<tauri::ipc::Response, String> {
    let size = size.clamp(32, 512);
    tauri::async_runtime::spawn_blocking(move || thumbs::thumbnail_with(Path::new(&path), size, quicklook.unwrap_or(true)))
        .await
        .map_err(|e| e.to_string())?
        .map(|bytes| tauri::ipc::Response::new(bytes.unwrap_or_default()))
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

/// (total, free) bytes where `path` lives. The git-drive mount reports
/// placeholder numbers (its FUSE layer has no statfs), so the repository's
/// real volume is measured instead.
fn measure(path: &Path) -> Option<(u64, u64)> {
    let status = git_drive_state::status::read_from(&git_drive_state::paths::standard_status_path()).ok();
    let measured = match status {
        Some(s) if !s.mount.is_empty() && path.starts_with(&s.mount) => PathBuf::from(s.repo),
        _ => path.to_path_buf(),
    };
    git_drive_state::storage::volume(&measured).ok().map(|v| (v.total, v.free))
}

/// The drive, the home folder and the volumes, for `Boot::places`.
fn anchors() -> Vec<places::Place> {
    let home = home();
    let drive = drive_mount();
    let mut out: Vec<places::Place> = drive.iter().map(|d| places::place_for(d, &home, Some(d))).collect();
    out.push(places::place_for(&home, &home, drive.as_deref()));
    for v in places::volumes(&home, drive.as_deref(), |_| None) {
        if v.kind != "drive" {
            out.push(places::Place { label: v.name, path: v.path, icon: "disk", missing: false });
        }
    }
    out
}

#[derive(Serialize)]
struct Sidebar {
    bookmarks: Vec<places::Place>,
    volumes: Vec<places::Volume>,
    /// The folder the Trash shows.
    trash: String,
}

fn bookmark_places(paths: &[PathBuf]) -> Vec<places::Place> {
    let home = home();
    let drive = drive_mount();
    paths.iter().map(|p| places::place_for(p, &home, drive.as_deref())).collect()
}

/// The sidebar: the bookmarks (the defaults until the user changes them),
/// the mounted volumes and the Trash. Read again when the window comes back.
#[tauri::command]
async fn sidebar() -> Result<Sidebar, String> {
    blocking(|| {
        let home = home();
        let drive = drive_mount();
        let paths: Vec<PathBuf> = match places::load_bookmarks(&places::bookmarks_file()) {
            Some(saved) => saved.into_iter().map(PathBuf::from).collect(),
            None => places::default_bookmarks(&home, drive.as_deref()),
        };
        Ok(Sidebar {
            bookmarks: bookmark_places(&paths),
            volumes: volumes(&home, drive.as_deref()),
            trash: places::trash_files(&places::trash_root(&home)).display().to_string(),
        })
    })
    .await
}

/// Saves the bookmarks in this order; returns them as the sidebar shows them.
#[tauri::command]
async fn save_bookmarks(paths: Vec<String>) -> Result<Vec<places::Place>, String> {
    blocking(move || {
        places::save_bookmarks(&places::bookmarks_file(), &paths).map_err(|e| t!("main.bookmarks_failed", error = i18n::io_text(&e)))?;
        Ok(bookmark_places(&to_paths(paths)))
    })
    .await
}

#[derive(Serialize)]
struct Computer {
    dirs: Vec<places::Place>,
    volumes: Vec<places::Volume>,
}

/// The Computer view: the home's standard folders and the volumes with their space.
#[tauri::command]
async fn computer() -> Result<Computer, String> {
    blocking(|| {
        let home = home();
        Ok(Computer { dirs: places::standard_dirs(&home), volumes: volumes(&home, drive_mount().as_deref()) })
    })
    .await
}

/// Puts an item of the Trash back where it was deleted from (Linux; on
/// macOS that record is Finder's own).
#[tauri::command]
async fn put_back(path: String) -> Result<String, String> {
    if cfg!(target_os = "macos") {
        return Err(t!("main.put_back_macos"));
    }
    blocking(move || places::put_back(&places::trash_root(&home()), Path::new(&path)).map(|p| p.display().to_string())).await
}

/// Deletes everything in the Trash for good; returns how many items went.
#[tauri::command]
async fn empty_trash(state: State<'_, AppState>, task: u64) -> Result<usize, String> {
    let task = state.tasks.start(task);
    blocking(move || places::empty_trash(&places::trash_root(&home()), &task)).await
}

/// Properties of each of `paths` (what `lstat` says, in the dialog's terms).
#[tauri::command]
async fn properties(paths: Vec<String>) -> Result<Vec<props::Info>, String> {
    blocking(move || to_paths(paths).iter().map(|p| props::info(p).map_err(|e| t!("main.item_error", path = p.display(), error = i18n::io_text(&e)))).collect()).await
}

/// What `paths` hold in all: bytes and entries (folders walked, links not followed).
#[tauri::command]
async fn total_size(state: State<'_, AppState>, paths: Vec<String>, task: u64) -> Result<(u64, u64), String> {
    let task = state.tasks.start(task);
    blocking(move || tasks::measure(&to_paths(paths), &task).map_err(|_| tasks::CANCELLED.to_string())).await
}

/// Changes the permissions of `paths` (with `recursive`, of what folders hold too); how many changed.
#[tauri::command]
async fn set_mode(state: State<'_, AppState>, paths: Vec<String>, change: props::ModeChange, recursive: bool, task: u64) -> Result<usize, String> {
    let task = state.tasks.start(task);
    blocking(move || props::set_mode(&to_paths(paths), change, recursive, &task)).await
}

/// Removes the quarantine flag of a download (macOS).
#[tauri::command]
async fn remove_quarantine(path: String) -> Result<(), String> {
    blocking(move || props::remove_quarantine(Path::new(&path))).await
}

/// An image's metadata; null for a file that is no image.
#[tauri::command]
async fn image_meta(path: String) -> Result<Option<meta::ImageMeta>, String> {
    blocking(move || meta::read(Path::new(&path))).await
}

/// Changes the metadata of the images among `paths`; how many changed.
#[tauri::command]
async fn write_image_meta(state: State<'_, AppState>, paths: Vec<String>, edit: meta::MetaEdit, task: u64) -> Result<usize, String> {
    let task = state.tasks.start(task);
    blocking(move || meta::write_many(&to_paths(paths), &edit, &task)).await
}

/// The language of the backend's messages (the window's, from Settings or the system).
#[tauri::command]
fn set_language(language: String) {
    i18n::set(i18n::Lang::from_tag(&language));
}

/// How long the drive's mount and the mount table are trusted.
const MOUNTS_FRESH: Duration = Duration::from_secs(10);

/// Up to four files of a folder for the previews on its icon, by the rules
/// of its place (`folder_preview::rules_for`): nothing for a mount point or
/// a network volume, the drive's limits inside the drive. `request` names
/// the scan, for `cancel_folder_preview`.
#[tauri::command]
async fn folder_preview(state: State<'_, AppState>, path: String, request: u64) -> Result<folder_preview::Picks, String> {
    let (drive, mounts) = {
        let mut cache = state.mounts.lock().unwrap();
        let stale = cache.as_ref().is_none_or(|(at, _, _)| at.elapsed() > MOUNTS_FRESH);
        if stale {
            *cache = Some((Instant::now(), drive_mount().and_then(|d| std::fs::canonicalize(d).ok()), places::read_mounts()));
        }
        let (_, d, m) = cache.as_ref().unwrap();
        (d.clone(), m.clone())
    };
    let running = state.previews.start(request);
    blocking(move || {
        use std::os::unix::fs::MetadataExt;
        let dir = std::fs::canonicalize(&path).map_err(|e| i18n::io_text(&e))?;
        // A mount point: its device differs from its parent's.
        let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
        let is_mount_point = dir.parent().is_none_or(|parent| dev(&dir) != dev(parent));
        let Some(limits) = folder_preview::rules_for(&dir, drive.as_deref(), &mounts, is_mount_point) else {
            return Ok(folder_preview::Picks::default());
        };
        folder_preview::pick(&dir, &limits, Instant::now() + limits.budget, &|| running.is_cancelled()).map_err(|e| i18n::io_text(&e))
    })
    .await
}

/// Stops a folder's scan at its next check (it answers `cancelled`).
#[tauri::command]
fn cancel_folder_preview(state: State<'_, AppState>, request: u64) {
    state.previews.cancel_if_running(request);
}

/// The progress of every file operation under way, by id.
#[tauri::command]
fn task_progress(state: State<'_, AppState>) -> HashMap<u64, tasks::Snapshot> {
    state.tasks.snapshots()
}

/// Stops the file operation `id` at its next chunk or entry.
#[tauri::command]
fn cancel_task(state: State<'_, AppState>, id: u64) {
    state.tasks.cancel(id);
}

/// Where a path typed in the location field leads, and whether it is a folder.
#[derive(Serialize)]
struct Resolved {
    path: String,
    is_dir: bool,
}

#[tauri::command]
async fn resolve_path(path: String, base: String) -> Result<Resolved, String> {
    blocking(move || {
        let (path, is_dir) = fsmodel::resolve_typed(&path, &home(), Path::new(&base))?;
        Ok(Resolved { path: path.display().to_string(), is_dir })
    })
    .await
}

/// The apps that open `path`, its default first, with their icons (Open With).
#[tauri::command]
async fn open_with_apps(window: tauri::WebviewWindow, path: String) -> Result<Vec<openwith::AppChoice>, String> {
    let path = PathBuf::from(path);
    // AppKit draws the icons on any thread; GTK's icon theme lives on the main one.
    if cfg!(target_os = "macos") {
        blocking(move || Ok(openwith::apps_for(&path))).await
    } else {
        on_main(&window, move |_| openwith::apps_for(&path)).await
    }
}

/// Opens `paths` with the app `id`; `always` also makes it the default for
/// the first file's type (Always Open With).
#[tauri::command]
async fn open_with(window: tauri::WebviewWindow, paths: Vec<String>, id: String, always: bool) -> Result<(), String> {
    let paths = to_paths(paths);
    on_main(&window, move |_| {
        if always {
            if let Some(first) = paths.first() {
                openwith::set_default(first, &id)?;
            }
        }
        openwith::open_with(&paths, &id)
    })
    .await?
}

/// Zips `paths` beside them; the archive's path.
#[tauri::command]
async fn compress(state: State<'_, AppState>, paths: Vec<String>, task: u64) -> Result<String, String> {
    let task = state.tasks.start(task);
    blocking(move || archive::compress(&to_paths(paths), &task).map(|p| p.display().to_string())).await
}

/// Extracts an archive beside it; where its items landed.
#[tauri::command]
async fn extract(state: State<'_, AppState>, path: String, task: u64) -> Result<Vec<String>, String> {
    let task = state.tasks.start(task);
    blocking(move || archive::extract(Path::new(&path), &task).map(|v| v.iter().map(|p| p.display().to_string()).collect())).await
}

/// Makes the image at `path` the desktop picture.
#[tauri::command]
async fn set_desktop_picture(window: tauri::WebviewWindow, path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    #[cfg(target_os = "macos")]
    {
        on_main(&window, move |_| match objc2::MainThreadMarker::new() {
            Some(mtm) => wallpaper::set(&path, mtm),
            None => Err(t!("main.wallpaper_thread")),
        })
        .await?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        blocking(move || wallpaper::set(&path)).await
    }
}

fn session(state: &AppState, id: u32) -> Result<Arc<terminal::Session>, String> {
    state.terminals.lock().unwrap().get(&id).cloned().ok_or_else(|| t!("main.terminal_closed"))
}

/// Opens a terminal tab: the user's login shell in `cwd`, its output sent
/// raw to `on_data` (xterm.js decodes it) and its exit code to `on_exit`.
#[tauri::command]
fn term_open(
    state: State<'_, AppState>,
    cwd: String,
    cols: u16,
    rows: u16,
    on_data: Channel<tauri::ipc::InvokeResponseBody>,
    on_exit: Channel<Option<u32>>,
) -> Result<u32, String> {
    let (shell, args) = terminal::shell_for(std::env::var("SHELL").ok().as_deref(), cfg!(target_os = "macos"));
    let dir = Some(PathBuf::from(&cwd)).filter(|d| d.is_dir()).unwrap_or_else(home);
    let session = terminal::Session::spawn(
        &shell,
        &args,
        &dir,
        terminal::Size { cols, rows },
        move |bytes| {
            let _ = on_data.send(tauri::ipc::InvokeResponseBody::Raw(bytes.to_vec()));
        },
        move |code| {
            let _ = on_exit.send(code);
        },
    )?;
    let id = state.next_terminal.fetch_add(1, Ordering::Relaxed) as u32;
    state.terminals.lock().unwrap().insert(id, Arc::new(session));
    Ok(id)
}

/// Keys and pasted text, as xterm.js sends them.
#[tauri::command]
fn term_write(state: State<'_, AppState>, id: u32, data: String) -> Result<(), String> {
    session(&state, id)?.write(data.as_bytes())
}

/// The terminal's new size, sent once a resize settles; `redraw` (its width
/// changed) has the shell redraw its screen when it is at its prompt.
#[tauri::command]
fn term_resize(state: State<'_, AppState>, id: u32, cols: u16, rows: u16, redraw: Option<bool>) -> Result<(), String> {
    let session = session(&state, id)?;
    session.resize(terminal::Size { cols, rows })?;
    if redraw == Some(true) {
        session.redraw_at_prompt(Duration::from_millis(120));
    }
    Ok(())
}

/// What a tab's terminal is doing: its shell's folder, and the server an
/// `ssh` in front of the shell is connected to.
#[derive(Serialize)]
struct TermStatus {
    cwd: Option<String>,
    ssh: Option<String>,
    /// The command running in front of the shell, when one is.
    command: Option<String>,
}

#[tauri::command]
fn term_status(state: State<'_, AppState>, id: u32) -> Option<TermStatus> {
    let s = state.terminals.lock().unwrap().get(&id).cloned()?;
    let front = s.foreground();
    Some(TermStatus {
        cwd: s.cwd().map(|p| p.display().to_string()),
        ssh: front.as_deref().and_then(terminal::ssh_destination),
        command: front.and_then(|a| a.first().map(|p| Path::new(p).file_name().map_or_else(|| p.clone(), |n| n.to_string_lossy().into_owned()))),
    })
}

/// Ends a tab's shell (closing the tab, or after it exited on its own).
#[tauri::command]
fn term_close(state: State<'_, AppState>, id: u32) {
    state.terminals.lock().unwrap().remove(&id);
}

/// The shell's working folder: the tab's title, and where relative links point.
#[tauri::command]
fn term_cwd(state: State<'_, AppState>, id: u32) -> Option<String> {
    session(&state, id).ok()?.cwd().map(|p| p.display().to_string())
}

/// Types the dropped `paths` into a tab, quoted for the shell.
#[tauri::command]
fn term_paste_paths(state: State<'_, AppState>, id: u32, paths: Vec<String>) -> Result<(), String> {
    session(&state, id)?.write(terminal::paste_text(&paths).as_bytes())
}

/// Types `cd` to `path` and Return into a tab.
#[tauri::command]
fn term_cd(state: State<'_, AppState>, id: u32, path: String) -> Result<(), String> {
    session(&state, id)?.write(terminal::cd_text(&path).as_bytes())
}

/// For each path, whether it is a folder (`true`), a file (`false`) or not there (`None`):
/// which words of a terminal line are links.
#[tauri::command]
fn path_kinds(paths: Vec<String>) -> Vec<Option<bool>> {
    paths.iter().map(|p| std::fs::metadata(p).ok().map(|m| m.is_dir())).collect()
}

/// Opens a web link in the default browser (http and https only).
#[tauri::command]
fn open_url(url: String) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(t!("main.web_links_only"));
    }
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let child = std::process::Command::new(opener).arg(&url).spawn();
    child.as_ref().map_err(i18n::io_text)?;
    git_drive_state::reap_in_background(child);
    Ok(())
}

/// The `.app` bundle an executable runs from (macOS), the folder Finder shows as the app.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf)
}

/// This app's bundle, to drag into a privacy list of System Settings; `None` outside one.
#[tauri::command]
fn app_bundle() -> Option<String> {
    std::env::current_exe().ok().as_deref().and_then(bundle_of).map(|p| p.display().to_string())
}

/// Opens System Settings at Privacy & Security, Full Disk Access (macOS): no
/// app can grant it to itself nor ask for it with a prompt. The user turns
/// the app on there (adding it with + when it is not listed), and macOS
/// offers to reopen it.
#[tauri::command]
fn open_full_disk_access() -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err(t!("main.fda_macos_only"));
    }
    let child = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn();
    child.as_ref().map_err(i18n::io_text)?;
    git_drive_state::reap_in_background(child);
    Ok(())
}

/// Opens a file in its default app, or reveals it in the file manager.
#[tauri::command]
fn open_path(path: String, reveal: bool) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(t!("main.not_there", path = path.display()));
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
    child.as_ref().map_err(i18n::io_text)?;
    git_drive_state::reap_in_background(child);
    Ok(())
}

/// Paths that are never renamed or deleted from here: the home folder and
/// its standard folders, the drive, the volumes' mount points and the Trash.
/// (A bookmark is the user's own folder: it can go, as in Finder.)
fn protected_paths() -> Vec<PathBuf> {
    let home = home();
    let mut keep: Vec<PathBuf> = anchors().into_iter().map(|p| PathBuf::from(p.path)).collect();
    keep.extend(places::standard_dirs(&home).into_iter().map(|p| PathBuf::from(p.path)));
    let trash = places::trash_root(&home);
    keep.push(places::trash_files(&trash));
    keep.push(trash);
    keep
}

fn to_paths(paths: Vec<String>) -> Vec<PathBuf> {
    paths.into_iter().map(PathBuf::from).collect()
}

/// Joins a blocking file operation; a panic in it becomes an error message.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[tauri::command]
async fn new_folder(parent: String, name: String) -> Result<String, String> {
    blocking(move || ops::new_folder(Path::new(&parent), &name).map(|p| p.display().to_string())).await
}

/// Renames `path` within its folder; returns the new path.
#[tauri::command]
async fn rename(path: String, name: String) -> Result<String, String> {
    blocking(move || ops::rename(Path::new(&path), &name, &protected_paths()).map(|p| p.display().to_string())).await
}

/// Runs `f` on the main thread (AppKit and GTK live there) and returns its result.
async fn on_main<T: Send + 'static>(
    window: &tauri::WebviewWindow,
    f: impl FnOnce(&tauri::WebviewWindow) -> T + Send + 'static,
) -> Result<T, String> {
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    let w = window.clone();
    window
        .run_on_main_thread(move || {
            let _ = tx.try_send(f(&w));
        })
        .map_err(|e| e.to_string())?;
    rx.recv().await.ok_or_else(|| t!("main.window_closed"))
}

/// Drags `paths` out of the window; resolves when the drag ends, with whether
/// something took them (this window's drop event, Finder, Files, the desktop).
#[tauri::command]
async fn start_drag(window: tauri::WebviewWindow, paths: Vec<String>) -> Result<bool, String> {
    let paths = to_paths(paths);
    if paths.is_empty() {
        return Err(t!("drag.nothing"));
    }
    let keep = protected_paths();
    if let Some(p) = paths.iter().find(|p| ops::protected(p, &keep)) {
        return Err(t!("main.cant_move_here", path = p.display()));
    }
    let (tx, mut rx) = tauri::async_runtime::channel::<Result<bool, String>>(1);
    let ended = tx.clone();
    let w = window.clone();
    window
        .run_on_main_thread(move || {
            let started = native_drag::start(&w, &paths, move |dropped| {
                let _ = ended.try_send(Ok(dropped));
            });
            if let Err(e) = started {
                let _ = tx.try_send(Err(e));
            }
        })
        .map_err(|e| e.to_string())?;
    rx.recv().await.unwrap_or_else(|| Err(t!("main.drag_no_answer")))
}

/// What a drop of `sources` on the folder `dest` does, read as it happens:
/// the keys held and whether the items share the folder's volume.
#[tauri::command]
async fn drop_operation(window: tauri::WebviewWindow, sources: Vec<String>, dest: String) -> Result<dnd::DropOp, String> {
    let forced = on_main(&window, native_drag::forced_keys).await?;
    blocking(move || Ok(dnd::drop_operation(&to_paths(sources), Path::new(&dest), forced))).await
}

/// Copies, or moves when `cut`, into `dest`; returns where each item landed.
#[tauri::command]
async fn paste(state: State<'_, AppState>, sources: Vec<String>, dest: String, cut: bool, task: u64) -> Result<Vec<String>, String> {
    let task = state.tasks.start(task);
    blocking(move || {
        let landed = ops::paste(&to_paths(sources), Path::new(&dest), cut, &task)?;
        Ok(landed.iter().map(|p| p.display().to_string()).collect())
    })
    .await
}

#[tauri::command]
async fn trash(state: State<'_, AppState>, paths: Vec<String>, task: u64) -> Result<ops::Trashed, String> {
    let task = state.tasks.start(task);
    blocking(move || ops::trash(&to_paths(paths), drive_mount().as_deref(), &protected_paths(), &task)).await
}

#[tauri::command]
async fn delete_permanently(state: State<'_, AppState>, paths: Vec<String>, task: u64) -> Result<(), String> {
    let task = state.tasks.start(task);
    blocking(move || {
        let paths = to_paths(paths);
        let result = ops::delete_permanently(&paths, &protected_paths(), &task);
        // Items deleted from inside the Trash leave no record behind (what
        // went before an error went for good too).
        let gone: Vec<PathBuf> = paths.into_iter().filter(|p| std::fs::symlink_metadata(p).is_err()).collect();
        places::forget_trash_records(&places::trash_root(&home()), &gone);
        result
    })
    .await
}

/// Native glass behind the transparent webview: Liquid Glass on macOS 26+,
/// the classic vibrancy material before it (same as the Control Center).
#[cfg(target_os = "macos")]
fn apply_glass(window: &tauri::WebviewWindow, light: bool) -> &'static str {
    use window_vibrancy::{
        apply_liquid_glass, apply_vibrancy, LiquidGlassOptions, NSGlassEffectViewStyle, NSVisualEffectMaterial,
        NSVisualEffectState,
    };
    // The glass's tint follows the theme: night blue, or a cool white.
    let tint = if light { (246, 247, 253, 120) } else { (28, 26, 58, 150) };
    let glass = LiquidGlassOptions::new(NSGlassEffectViewStyle::Regular).tint_color(tint);
    if apply_liquid_glass(window, glass).is_ok() {
        return "liquid-glass";
    }
    let material = if light { NSVisualEffectMaterial::Sidebar } else { NSVisualEffectMaterial::HudWindow };
    match apply_vibrancy(window, material, Some(NSVisualEffectState::Active), None) {
        Ok(()) => "vibrancy",
        Err(_) => "none",
    }
}

/// The window's appearance: `light` (true or false) forces one, absent
/// follows the system's; the native glass is laid again with the matching tint.
#[tauri::command]
async fn set_appearance(window: tauri::WebviewWindow, light: Option<bool>, effective_light: bool) -> Result<(), String> {
    let theme = light.map(|l| if l { tauri::Theme::Light } else { tauri::Theme::Dark });
    window.set_theme(theme).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    on_main(&window, move |w| {
        let _ = window_vibrancy::clear_liquid_glass(w);
        let _ = window_vibrancy::clear_vibrancy(w);
        apply_glass(w, effective_light);
    })
    .await?;
    #[cfg(not(target_os = "macos"))]
    let _ = effective_light;
    Ok(())
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
    let state = AppState {
        search_generation: Arc::default(),
        glass: Mutex::new("none"),
        start,
        terminals: Mutex::default(),
        next_terminal: AtomicU64::new(1),
        tasks: Arc::default(),
        previews: Arc::default(),
        mounts: Mutex::new(None),
    };
    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            boot,
            accent,
            save_accent,
            list_dir,
            preview_kind,
            read_text,
            thumbnail,
            search,
            sidebar,
            save_bookmarks,
            computer,
            put_back,
            empty_trash,
            app_bundle,
            open_full_disk_access,
            open_with_apps,
            open_with,
            compress,
            extract,
            set_desktop_picture,
            term_open,
            term_write,
            term_resize,
            term_close,
            term_cwd,
            term_status,
            term_paste_paths,
            term_cd,
            path_kinds,
            open_url,
            open_path,
            new_folder,
            rename,
            paste,
            start_drag,
            drop_operation,
            trash,
            delete_permanently,
            task_progress,
            cancel_task,
            resolve_path,
            properties,
            total_size,
            set_mode,
            remove_quarantine,
            image_meta,
            write_image_meta,
            set_appearance,
            set_language,
            folder_preview,
            cancel_folder_preview
        ])
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
                *app.state::<AppState>().glass.lock().unwrap() = apply_glass(&window, false);
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

    #[test]
    fn the_app_bundle_holds_the_executable() {
        let exe = Path::new("/Users/u/Apps/git-drive Files.app/Contents/MacOS/git-drive-files");
        assert_eq!(bundle_of(exe), Some(PathBuf::from("/Users/u/Apps/git-drive Files.app")));
        assert_eq!(bundle_of(Path::new("/home/u/.local/bin/git-drive-files")), None, "a bare binary has no bundle");
    }
}
