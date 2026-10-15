//! The file manager's model of the filesystem: listing in natural order,
//! preview classification, bounded text reads and name search. Plain std,
//! tested without a webview; `main.rs` only adapts it to Tauri commands.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fs::{self, Metadata};
use std::io::{self, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    /// A folder, or a symlink to one: it opens like a folder.
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds.
    pub modified: Option<u64>,
    pub hidden: bool,
    /// Lowercase extension of a file; empty for folders and files without one.
    pub ext: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<Entry>,
}

/// Finder-like order: case-insensitive, digit runs compared by value
/// ("file2" before "file10"); equal values put fewer leading zeros first.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ia, mut ib) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let ra = take_digits(&mut ia);
                let rb = take_digits(&mut ib);
                let (ta, tb) = (ra.trim_start_matches('0'), rb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| ra.len().cmp(&rb.len()));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(ca), Some(cb)) => {
                let (la, lb) = (lower(ca), lower(cb));
                if la != lb {
                    return la.cmp(&lb);
                }
                ia.next();
                ib.next();
            }
        }
    }
}

fn take_digits(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut run = String::new();
    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
        run.push(c);
        it.next();
    }
    run
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Dot-files, plus (on macOS) items flagged hidden like `~/Library`.
fn is_hidden(name: &str, meta: Option<&Metadata>) -> bool {
    if name.starts_with('.') {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        const UF_HIDDEN: u32 = 0x8000;
        if meta.is_some_and(|m| m.st_flags() & UF_HIDDEN != 0) {
            return true;
        }
    }
    let _ = meta;
    false
}

fn unix_secs(meta: &Metadata) -> Option<u64> {
    meta.modified().ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn ext_of(path: &Path) -> String {
    path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The entry for `path`; `None` when it vanished or cannot be read.
pub fn entry_for(path: &Path, name: String) -> Option<Entry> {
    let meta = fs::symlink_metadata(path).ok()?;
    let hidden = is_hidden(&name, Some(&meta));
    let ft = meta.file_type();
    let (kind, shown) = if ft.is_symlink() {
        // A broken link still shows, with the link's own metadata.
        (EntryKind::Symlink, fs::metadata(path).unwrap_or(meta))
    } else if ft.is_dir() {
        (EntryKind::Dir, meta)
    } else {
        (EntryKind::File, meta)
    };
    let is_dir = shown.is_dir();
    Some(Entry {
        ext: if is_dir { String::new() } else { ext_of(path) },
        path: path.to_string_lossy().into_owned(),
        name,
        kind,
        is_dir,
        size: if is_dir { 0 } else { shown.len() },
        modified: unix_secs(&shown),
        hidden,
    })
}

fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name)));
}

/// The folder's entries, folders first, in natural order.
pub fn list_dir(dir: &Path, show_hidden: bool) -> io::Result<Listing> {
    let mut entries = Vec::new();
    for item in fs::read_dir(dir)?.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if let Some(entry) = entry_for(&item.path(), name) {
            if show_hidden || !entry.hidden {
                entries.push(entry);
            }
        }
    }
    sort_entries(&mut entries);
    Ok(Listing {
        path: dir.to_string_lossy().into_owned(),
        parent: dir.parent().map(|p| p.to_string_lossy().into_owned()),
        entries,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    Directory,
    Image,
    Video,
    Audio,
    Pdf,
    Text,
    Other,
}

const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "avif", "heic", "heif", "tif", "tiff"];
// Only containers the webviews play; others open in the default app.
const VIDEO: &[&str] = &["mp4", "m4v", "mov", "webm"];
const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "ogg", "oga", "opus", "flac", "aif", "aiff"];
const TEXT: &[&str] = &[
    "txt", "md", "markdown", "rst", "json", "jsonc", "yaml", "yml", "toml", "ini", "cfg", "conf", "env", "csv",
    "tsv", "log", "xml", "html", "htm", "css", "scss", "js", "mjs", "cjs", "ts", "tsx", "jsx", "rs", "py", "rb",
    "go", "java", "kt", "swift", "c", "h", "cpp", "hpp", "cc", "cs", "php", "sh", "bash", "zsh", "fish", "sql",
    "lua", "r", "dart", "vue", "svelte", "gradle", "properties", "lock", "plist", "srt", "vtt", "tex", "diff",
    "patch",
];

/// How the file can be previewed: by extension, then by content for files
/// without a known one (`Makefile`, `notes`).
pub fn classify(path: &Path) -> PreviewKind {
    if path.is_dir() {
        return PreviewKind::Directory;
    }
    let ext = ext_of(path);
    let ext = ext.as_str();
    if IMAGE.contains(&ext) {
        PreviewKind::Image
    } else if VIDEO.contains(&ext) {
        PreviewKind::Video
    } else if AUDIO.contains(&ext) {
        PreviewKind::Audio
    } else if ext == "pdf" {
        PreviewKind::Pdf
    } else if TEXT.contains(&ext) || looks_like_text(path) {
        PreviewKind::Text
    } else {
        PreviewKind::Other
    }
}

/// UTF-8 without NUL bytes in the first 8 KiB (a character cut at the end of
/// the sample is fine).
fn looks_like_text(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else { return false };
    let mut sample = Vec::with_capacity(8192);
    if file.take(8192).read_to_end(&mut sample).is_err() {
        return false;
    }
    if sample.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&sample) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TextPreview {
    pub text: String,
    pub truncated: bool,
    /// Size of the whole file.
    pub bytes: u64,
}

/// The first `max` bytes as text, cut back to a character boundary.
pub fn read_text(path: &Path, max: usize) -> io::Result<TextPreview> {
    let file = fs::File::open(path)?;
    let bytes = file.metadata()?.len();
    let mut buf = Vec::with_capacity(max.min(bytes as usize));
    file.take(max as u64).read_to_end(&mut buf)?;
    let truncated = (buf.len() as u64) < bytes;
    let text = match String::from_utf8(buf) {
        Ok(text) => text,
        Err(e) => {
            let (valid, cut_at_end) = (e.utf8_error().valid_up_to(), e.utf8_error().error_len().is_none());
            let mut raw = e.into_bytes();
            if cut_at_end {
                raw.truncate(valid);
            }
            String::from_utf8_lossy(&raw).into_owned()
        }
    };
    Ok(TextPreview { text, truncated, bytes })
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub max_results: usize,
    pub max_visited: usize,
    pub show_hidden: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        SearchOptions { max_results: 1000, max_visited: 200_000, show_hidden: false }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SearchResult {
    pub entries: Vec<Entry>,
    /// A limit stopped the search before it covered everything.
    pub truncated: bool,
    /// A newer search took over.
    pub cancelled: bool,
    pub visited: usize,
}

/// Entries under `root` whose name contains `query` (case-insensitive),
/// breadth-first so closer matches come first. `.git` is never entered;
/// hidden items only with `show_hidden`. `cancelled` is polled per folder;
/// `on_found` sees each match as soon as it is found (progressive results).
pub fn search(
    root: &Path,
    query: &str,
    opts: &SearchOptions,
    cancelled: &dyn Fn() -> bool,
    on_found: &mut dyn FnMut(&Entry),
) -> SearchResult {
    let mut out = SearchResult::default();
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return out;
    }
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        if cancelled() {
            out.cancelled = true;
            return out;
        }
        let Ok(items) = fs::read_dir(&dir) else { continue };
        for item in items.flatten() {
            out.visited += 1;
            if out.visited > opts.max_visited {
                out.truncated = true;
                return out;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let is_dir = item.file_type().is_ok_and(|t| t.is_dir());
            // Only folders pay for the metadata (the macOS hidden flag).
            let meta = if is_dir { item.metadata().ok() } else { None };
            if !opts.show_hidden && is_hidden(&name, meta.as_ref()) {
                continue;
            }
            if is_dir {
                queue.push_back(item.path());
            }
            if name.to_lowercase().contains(&needle) {
                if out.entries.len() >= opts.max_results {
                    out.truncated = true;
                    return out;
                }
                if let Some(entry) = entry_for(&item.path(), name) {
                    on_found(&entry);
                    out.entries.push(entry);
                }
            }
        }
    }
    out
}

#[derive(Debug, Clone, Serialize)]
pub struct Place {
    pub label: String,
    pub path: String,
    /// Icon id for the frontend: drive, home, desktop, documents, downloads, computer.
    pub icon: &'static str,
}

/// Sidebar places: the git-drive mount first, then home folders that exist,
/// then the root.
pub fn places(home: &Path, drive: Option<&Path>) -> Vec<Place> {
    let place = |label: &str, path: &Path, icon| Place { label: label.to_string(), path: path.to_string_lossy().into_owned(), icon };
    let mut out = Vec::new();
    if let Some(drive) = drive.filter(|d| d.is_dir()) {
        let label = drive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Drive".into());
        out.push(place(&label, drive, "drive"));
    }
    out.push(place("Home", home, "home"));
    for (dir, icon) in [("Desktop", "desktop"), ("Documents", "documents"), ("Downloads", "downloads")] {
        let path = home.join(dir);
        if path.is_dir() {
            out.push(place(dir, &path, icon));
        }
    }
    out.push(place("Computer", Path::new("/"), "computer"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn names(listing: &Listing) -> Vec<&str> {
        listing.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn natural_order_compares_numbers_by_value_and_ignores_case() {
        let mut v = vec!["file10.txt", "File2.txt", "file1.txt", "alpha", "Beta", "file02.txt"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["alpha", "Beta", "file1.txt", "File2.txt", "file02.txt", "file10.txt"]);
    }

    #[test]
    fn listing_puts_folders_first_and_hides_dotfiles_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("zeta")).unwrap();
        fs::create_dir(dir.path().join(".cache")).unwrap();
        fs::write(dir.path().join("b10.md"), "x").unwrap();
        fs::write(dir.path().join("b2.md"), "hello").unwrap();
        fs::write(dir.path().join(".env"), "s").unwrap();
        std::os::unix::fs::symlink(dir.path().join("zeta"), dir.path().join("alink")).unwrap();

        let l = list_dir(dir.path(), false).unwrap();
        assert_eq!(names(&l), vec!["alink", "zeta", "b2.md", "b10.md"], "a link to a folder sorts with folders");
        let link = &l.entries[0];
        assert_eq!(link.kind, EntryKind::Symlink);
        assert!(link.is_dir, "the link points at a folder");
        let b2 = l.entries.iter().find(|e| e.name == "b2.md").unwrap();
        assert_eq!((b2.size, b2.ext.as_str(), b2.kind), (5, "md", EntryKind::File));
        assert!(b2.modified.is_some());

        let all = list_dir(dir.path(), true).unwrap();
        assert_eq!(all.entries.len(), 6);
        assert!(all.entries.iter().any(|e| e.name == ".env" && e.hidden));
        assert_eq!(all.parent.as_deref(), dir.path().parent().and_then(|p| p.to_str()));
    }

    #[test]
    fn files_are_classified_for_preview_by_extension_then_content() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes");
        fs::write(&text, "plain words, no extension\n").unwrap();
        let bin = dir.path().join("blob");
        fs::write(&bin, [0u8, 159, 146, 150, 0, 1]).unwrap();
        assert_eq!(classify(Path::new("a/photo.JPG")), PreviewKind::Image);
        assert_eq!(classify(Path::new("clip.mov")), PreviewKind::Video);
        assert_eq!(classify(Path::new("song.flac")), PreviewKind::Audio);
        assert_eq!(classify(Path::new("doc.pdf")), PreviewKind::Pdf);
        assert_eq!(classify(Path::new("main.rs")), PreviewKind::Text);
        assert_eq!(classify(&text), PreviewKind::Text, "sniffed");
        assert_eq!(classify(&bin), PreviewKind::Other, "binary content");
        assert_eq!(classify(dir.path()), PreviewKind::Directory);
    }

    #[test]
    fn text_preview_is_truncated_on_a_character_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.txt");
        fs::write(&path, "aé".repeat(10)).unwrap(); // 30 bytes, 'é' is 2 bytes
        let full = read_text(&path, 1024).unwrap();
        assert_eq!((full.text.len(), full.truncated, full.bytes), (30, false, 30));
        let cut = read_text(&path, 8).unwrap(); // byte 8 falls inside an 'é'
        assert!(cut.truncated);
        assert_eq!(cut.text, "aéaé".to_string() + "a", "{:?}", cut.text);
    }

    #[test]
    fn search_finds_nested_names_case_insensitively_and_skips_git_and_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("docs/Reports/2026")).unwrap();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("docs/Reports/2026/Q3-report.pdf"), "x").unwrap();
        fs::write(root.join("docs/readme.md"), "x").unwrap();
        fs::write(root.join(".git/objects/report-pack"), "x").unwrap();
        fs::write(root.join(".hidden/report.txt"), "x").unwrap();

        let r = search(root, "REPORT", &SearchOptions::default(), &|| false, &mut |_| {});
        let mut found: Vec<&str> = r.entries.iter().map(|e| e.name.as_str()).collect();
        found.sort();
        assert_eq!(found, vec!["Q3-report.pdf", "Reports"], "folders match too");
        assert!(!r.truncated && !r.cancelled);

        let with_hidden = search(root, "report", &SearchOptions { show_hidden: true, ..Default::default() }, &|| false, &mut |_| {});
        assert!(with_hidden.entries.iter().any(|e| e.name == "report.txt"));
        assert!(!with_hidden.entries.iter().any(|e| e.name == "report-pack"), ".git is never searched");
    }

    #[test]
    fn search_stops_at_its_limits_and_when_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..30 {
            fs::write(dir.path().join(format!("hit{}.txt", i)), "x").unwrap();
        }
        let limited = search(dir.path(), "hit", &SearchOptions { max_results: 10, ..Default::default() }, &|| false, &mut |_| {});
        assert_eq!(limited.entries.len(), 10);
        assert!(limited.truncated);
        let cancelled = search(dir.path(), "hit", &SearchOptions::default(), &|| true, &mut |_| {});
        assert!(cancelled.cancelled);
        assert!(search(dir.path(), "  ", &SearchOptions::default(), &|| false, &mut |_| {}).entries.is_empty(), "blank query");
    }

    #[test]
    fn search_reports_each_match_as_it_is_found() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("a/b")).unwrap();
        fs::write(dir.path().join("logo.png"), "x").unwrap();
        fs::write(dir.path().join("a/b/old-logo.svg"), "x").unwrap();
        let mut seen = Vec::new();
        let r = search(dir.path(), "logo", &SearchOptions::default(), &|| false, &mut |e| seen.push(e.name.clone()));
        let names: Vec<String> = r.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(seen, names, "the reporter sees every result, in order");
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn places_list_existing_standard_folders_and_the_drive() {
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("Documents")).unwrap();
        fs::create_dir(home.path().join("Downloads")).unwrap();
        let drive = home.path().join("GitDrive");
        fs::create_dir(&drive).unwrap();
        let p = places(home.path(), Some(&drive));
        let labels: Vec<&str> = p.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, vec!["GitDrive", "Home", "Documents", "Downloads", "Computer"], "missing folders are left out");
        assert_eq!(p[0].icon, "drive");
        assert_eq!(p.last().unwrap().path, "/");
    }
}
