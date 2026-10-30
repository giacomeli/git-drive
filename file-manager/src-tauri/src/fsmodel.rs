//! The file manager's model of the filesystem: listing in natural order,
//! preview classification, bounded text reads and name search. Plain std,
//! tested without a webview; `main.rs` only adapts it to Tauri commands.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::fs::{self, Metadata};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
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
    /// Unix seconds; `None` where the file system keeps no birth time.
    pub created: Option<u64>,
    pub hidden: bool,
    /// Lowercase extension of a file; empty for folders and files without one.
    pub ext: String,
    /// The icon view can draw a thumbnail of it (`thumbs::source_for`).
    pub thumb: bool,
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
    let ext = if is_dir { String::new() } else { ext_of(path) };
    let size = if is_dir { 0 } else { shown.len() };
    Some(Entry {
        thumb: !is_dir && crate::thumbs::source_for(&ext, size).is_some(),
        ext,
        path: path.to_string_lossy().into_owned(),
        name,
        kind,
        is_dir,
        size,
        modified: unix_secs(&shown),
        created: shown.created().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()),
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
    /// Text the window renders (headings, lists, tables).
    Markdown,
    /// Text the window formats when it parses.
    Json,
    /// A 3D model the window draws (STL, 3MF, OBJ).
    Model,
    Other,
}

const IMAGE: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "avif", "heic", "heif", "tif", "tiff"];
// Only containers the webviews play; others open in the default app.
const VIDEO: &[&str] = &["mp4", "m4v", "mov", "webm"];
const AUDIO: &[&str] = &["mp3", "m4a", "aac", "wav", "ogg", "oga", "opus", "flac", "aif", "aiff"];
const MARKDOWN: &[&str] = &["md", "markdown", "mdown", "mkd"];
// Strict JSON only: `.jsonc` keeps its comments as written (plain text).
const JSON: &[&str] = &["json", "geojson", "webmanifest"];
const MODEL: &[&str] = &["stl", "3mf", "obj"];
const TEXT: &[&str] = &[
    "txt", "rst", "jsonc", "yaml", "yml", "toml", "ini", "cfg", "conf", "env", "csv",
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
    } else if MARKDOWN.contains(&ext) {
        PreviewKind::Markdown
    } else if JSON.contains(&ext) {
        PreviewKind::Json
    } else if MODEL.contains(&ext) {
        PreviewKind::Model
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

/// What a text preview reads when the window names no budget.
pub const TEXT_PREVIEW_BYTES: usize = 256 * 1024;
/// The most a text preview ever reads, whatever the window asks (Quick
/// Look's largest setting).
pub const TEXT_MAX_BYTES: usize = 100 << 20;

/// The bytes a text preview reads: the window's budget (a Markdown or JSON
/// file within the preview's size limit is read whole), else the default.
pub fn text_budget(max: Option<u64>) -> usize {
    match max {
        None => TEXT_PREVIEW_BYTES,
        Some(n) => n.clamp(1, TEXT_MAX_BYTES as u64) as usize,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TextPreview {
    pub text: String,
    pub truncated: bool,
    /// Binary data follows: the text stops before it.
    pub binary: bool,
    /// Size of the whole file.
    pub bytes: u64,
}

/// The first `max` bytes as text, cut back to a character boundary. Text
/// never holds a NUL byte: from the first one on, the file is binary data
/// (an installer's payload after its script), which is left out. Laid out as
/// text, that garbage froze the preview for over a second.
pub fn read_text(path: &Path, max: usize) -> io::Result<TextPreview> {
    let file = fs::File::open(path)?;
    let bytes = file.metadata()?.len();
    let mut buf = Vec::with_capacity(max.min(bytes as usize));
    file.take(max as u64).read_to_end(&mut buf)?;
    let mut truncated = (buf.len() as u64) < bytes;
    let binary = match buf.iter().position(|&b| b == 0) {
        Some(nul) => {
            buf.truncate(nul);
            truncated = true;
            true
        }
        None => false,
    };
    let mut text = match String::from_utf8(buf) {
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
    if binary {
        drop_garbage_tail(&mut text);
    }
    Ok(TextPreview { text, truncated, binary, bytes })
}

/// Drops the last lines while they hold a replacement or control character:
/// binary data seldom starts with its NUL byte, so some of it comes before.
fn drop_garbage_tail(text: &mut String) {
    let garbage = |c: char| c == '\u{FFFD}' || (c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{c}' | '\u{1b}'));
    loop {
        let body = text.strip_suffix('\n').unwrap_or(text);
        let start = body.rfind('\n').map_or(0, |i| i + 1);
        if start == text.len() || !body[start..].chars().any(garbage) {
            break;
        }
        text.truncate(start);
    }
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

/// A path typed in the location field: `~` (alone or before a slash) is the
/// home folder, a relative path starts at `base` (the folder in view), `.`
/// and `..` resolve by name (the folder above a link is the link's own) and
/// trailing slashes go. Returns the path and whether it is a folder (a link
/// to one counts); an error says what is not there.
pub fn resolve_typed(input: &str, home: &Path, base: &Path) -> Result<(PathBuf, bool), String> {
    let typed = input.trim();
    if typed.is_empty() {
        return Err(t!("fsmodel.type_a_path"));
    }
    let expanded = match typed.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => base.join(typed),
    };
    let mut path = PathBuf::from("/");
    for part in expanded.components() {
        match part {
            std::path::Component::ParentDir => {
                path.pop();
            }
            std::path::Component::Normal(name) => path.push(name),
            _ => {}
        }
    }
    match std::fs::metadata(&path) {
        Ok(meta) => Ok((path, meta.is_dir())),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(t!("fsmodel.permission_denied", path = path.display())),
        Err(_) => Err(t!("fsmodel.nothing_at", path = path.display())),
    }
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
    fn entries_say_whether_the_icon_view_can_draw_a_thumbnail() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("photo.JPG"), "x").unwrap();
        fs::write(dir.path().join("main.rs"), "x").unwrap();
        fs::create_dir(dir.path().join("pics.png")).unwrap();
        let l = list_dir(dir.path(), false).unwrap();
        let thumb = |name: &str| l.entries.iter().find(|e| e.name == name).unwrap().thumb;
        assert!(thumb("photo.JPG"));
        assert!(!thumb("main.rs"));
        assert!(!thumb("pics.png"), "a folder never has one");
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
        assert_eq!(classify(Path::new("README.md")), PreviewKind::Markdown);
        assert_eq!(classify(Path::new("notes.MARKDOWN")), PreviewKind::Markdown);
        assert_eq!(classify(Path::new("package.json")), PreviewKind::Json);
        assert_eq!(classify(Path::new("map.geojson")), PreviewKind::Json);
        assert_eq!(classify(Path::new("tsconfig.jsonc")), PreviewKind::Text, "comments: shown as written");
        for model in ["part.stl", "Bracket.STL", "plate.3mf", "chair.obj"] {
            assert_eq!(classify(Path::new(model)), PreviewKind::Model, "{model}");
        }
        assert_eq!(classify(&text), PreviewKind::Text, "sniffed");
        assert_eq!(classify(&bin), PreviewKind::Other, "binary content");
        assert_eq!(classify(dir.path()), PreviewKind::Directory);
    }

    #[test]
    fn the_text_budget_defaults_and_is_capped() {
        assert_eq!(text_budget(None), TEXT_PREVIEW_BYTES);
        assert_eq!(text_budget(Some(1 << 20)), 1 << 20);
        assert_eq!(text_budget(Some(0)), 1, "at least a byte");
        assert_eq!(text_budget(Some(u64::MAX)), TEXT_MAX_BYTES, "never past the cap");
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
    fn binary_data_after_a_text_header_is_left_out() {
        // A self-extracting installer (Miniforge3.sh): a script, then a binary
        // payload whose garbage froze the preview's layout for over a second.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("installer.sh");
        let header = "#!/bin/sh\necho installing\n@@END_HEADER@@\n";
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend([0x9c, 0xff, b'\n', 0x41, 0x8f, 0x01, 0x00, 0x02, 0xfe, b'\n', 0x00, 0x7f]);
        fs::write(&path, &bytes).unwrap();
        let preview = read_text(&path, 1024).unwrap();
        assert_eq!(preview.text, header, "only the script, without the garbage lines before the first NUL");
        assert!(preview.binary && preview.truncated);
        assert_eq!(preview.bytes, bytes.len() as u64);
    }

    #[test]
    fn a_file_of_binary_data_previews_no_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob.txt");
        fs::write(&path, [0x00, 0x13, 0xff, 0x00, b'a']).unwrap();
        let preview = read_text(&path, 1024).unwrap();
        assert_eq!(preview.text, "");
        assert!(preview.binary);
    }

    #[test]
    fn a_stray_invalid_byte_stays_a_replacement_character() {
        // Latin-1 text: one bad byte must not hide the rest of the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin1.csv");
        fs::write(&path, b"caf\xe9 com leite\nsegunda linha\n").unwrap();
        let preview = read_text(&path, 1024).unwrap();
        assert_eq!(preview.text, "caf\u{FFFD} com leite\nsegunda linha\n");
        assert!(!preview.binary && !preview.truncated);
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
    fn typed_paths_resolve_like_a_shell_would() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        fs::create_dir_all(home.join("Documents/Work")).unwrap();
        fs::write(home.join("Documents/notes.md"), "n").unwrap();
        let docs = home.join("Documents");
        assert_eq!(resolve_typed("~", &home, Path::new("/")).unwrap(), (home.clone(), true));
        assert_eq!(resolve_typed(" ~/Documents/ ", &home, Path::new("/")).unwrap(), (docs.clone(), true));
        assert_eq!(resolve_typed("Work", &home, &docs).unwrap(), (docs.join("Work"), true), "relative to the folder in view");
        assert_eq!(resolve_typed("../Documents/./notes.md", &home, &docs).unwrap(), (docs.join("notes.md"), false));
        assert_eq!(resolve_typed(&format!("{}//Work", docs.display()), &home, Path::new("/")).unwrap(), (docs.join("Work"), true));
        assert!(resolve_typed("~/Nope", &home, Path::new("/")).unwrap_err().contains("no folder or file"));
        assert!(resolve_typed("   ", &home, Path::new("/")).is_err());
        assert_eq!(resolve_typed("~user", &home, &docs).unwrap_err(), format!("There is no folder or file at {}.", docs.join("~user").display()), "only ~ and ~/ are the home");
    }
}
