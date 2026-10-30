//! Previews of what a folder holds, for its icon in the icon view (Dolphin
//! lays them over the folder): up to four of its own files whose kind draws
//! a thumbnail, pictures first, then videos, then documents, in natural
//! order. The cost is bounded in any folder: so many entries read (names and
//! types only), so many `lstat`s, a deadline, and a cancel checked as it
//! goes. The rules depend on the place: the drive (a FUSE mount whose loop
//! serves one request at a time) gets fewer reads and pictures only, decoded
//! in this process (Quick Look would write a PNG of decrypted content to
//! `$TMPDIR`); a mount point, a network volume or another FUSE file system
//! gets no preview at all.

use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::fsmodel::{entry_for, natural_cmp, Entry};
use crate::places::Mount;
use crate::thumbs::{source_for, Source};

pub const MAX_PICKS: usize = 4;

/// How far a scan may go in one place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// Pictures decoded here only (no Quick Look).
    pub raster_only: bool,
    pub max_file_bytes: u64,
    /// Entries read from the folder (names and types, no `stat`).
    pub max_scanned: usize,
    /// `lstat`s of candidates.
    pub max_stats: usize,
    pub budget: Duration,
}

impl Limits {
    /// A local volume: what the file thumbnails draw.
    pub const LOCAL: Limits = Limits { raster_only: false, max_file_bytes: u64::MAX, max_scanned: 2_000, max_stats: 48, budget: Duration::from_millis(250) };
    /// The drive: pictures only, small enough to read at once, fewer reads.
    pub const DRIVE: Limits = Limits { raster_only: true, max_file_bytes: 16 << 20, max_scanned: 1_000, max_stats: 24, budget: Duration::from_millis(300) };
}

/// What a scan found.
#[derive(Debug, Default, Serialize)]
pub struct Picks {
    pub entries: Vec<Entry>,
    /// The window asks for these thumbnails without Quick Look.
    pub raster_only: bool,
    pub scanned: usize,
    /// It stopped at a limit or at the deadline: there may be better picks.
    pub truncated: bool,
    pub cancelled: bool,
}

/// Pictures, videos, documents: the order previews take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    Picture,
    Video,
    Document,
}

const PICTURES: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "heic", "heif", "avif", "tif", "tiff", "psd"];
const VIDEOS: &[&str] = &["mp4", "m4v", "mov"];

/// The family of a file by its (lowercase) extension, when its kind draws a
/// thumbnail at all (`thumbs::source_for`, any size). SVG is left out: only
/// the webview draws it, from the file.
pub fn family(ext: &str) -> Option<Family> {
    source_for(ext, 0)?;
    Some(if PICTURES.contains(&ext) {
        Family::Picture
    } else if VIDEOS.contains(&ext) {
        Family::Video
    } else {
        Family::Document
    })
}

/// Whether a file of this kind and size can be drawn under `limits`.
fn drawable(ext: &str, size: u64, limits: &Limits) -> bool {
    if size == 0 || size > limits.max_file_bytes {
        return false;
    }
    match source_for(ext, size) {
        Some(Source::Raster) => true,
        #[cfg(target_os = "macos")]
        Some(Source::QuickLook) => !limits.raster_only,
        None => false,
    }
}

fn ext_of(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Up to four previews of `dir`'s own files, within `limits`, stopping at
/// `deadline` or once `cancelled` says so.
pub fn pick(dir: &Path, limits: &Limits, deadline: Instant, cancelled: &dyn Fn() -> bool) -> io::Result<Picks> {
    let mut picks = Picks { raster_only: limits.raster_only, ..Picks::default() };
    let mut candidates: Vec<(Family, String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        if picks.scanned.is_multiple_of(64) {
            if cancelled() {
                picks.cancelled = true;
                return Ok(picks);
            }
            if Instant::now() >= deadline {
                picks.truncated = true;
                break;
            }
        }
        if picks.scanned >= limits.max_scanned {
            picks.truncated = true;
            break;
        }
        picks.scanned += 1;
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !entry.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if let Some(f) = family(&ext_of(&name)) {
            candidates.push((f, name, entry.path()));
        }
    }
    candidates.sort_by(|a, b| match a.0.cmp(&b.0) {
        Ordering::Equal => natural_cmp(&a.1, &b.1),
        other => other,
    });
    for (stats, (_, name, path)) in candidates.into_iter().enumerate() {
        if picks.entries.len() >= MAX_PICKS {
            break;
        }
        if cancelled() {
            picks.cancelled = true;
            picks.entries.clear();
            return Ok(picks);
        }
        if stats >= limits.max_stats || Instant::now() >= deadline {
            picks.truncated = true;
            break;
        }
        let Some(e) = entry_for(&path, name) else { continue };
        if e.hidden || !drawable(&e.ext, e.size, limits) {
            continue;
        }
        picks.entries.push(e);
    }
    Ok(picks)
}

/// File systems whose reads go over a network or through another process:
/// their folders get no previews.
const REMOTE: &[&str] = &["smbfs", "nfs", "nfs4", "afpfs", "webdav", "cifs", "smb3", "fuse.sshfs", "sshfs", "davfs", "fuse.rclone", "9p"];

/// The rules for a folder (its canonical path): none for a mount point, a
/// network volume or a FUSE file system other than the drive; the drive's
/// inside the drive; a local volume's elsewhere. A path's mount is the one
/// of the longest prefix.
pub fn rules_for(dir: &Path, drive: Option<&Path>, mounts: &[Mount], is_mount_point: bool) -> Option<Limits> {
    if is_mount_point {
        return None;
    }
    if drive.is_some_and(|d| dir.starts_with(d)) {
        return Some(Limits::DRIVE);
    }
    let mount = mounts.iter().filter(|m| dir.starts_with(&m.on)).max_by_key(|m| m.on.len());
    if let Some(m) = mount {
        let fs = m.fstype.as_str();
        if REMOTE.contains(&fs) || fs.starts_with("fuse") || fs.contains("macfuse") || fs.contains("osxfuse") {
            return None;
        }
    }
    Some(Limits::LOCAL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write(path: &Path, bytes: usize) {
        fs::write(path, vec![7u8; bytes]).unwrap();
    }

    fn names(p: &Picks) -> Vec<&str> {
        p.entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn pictures_come_first_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["img10.jpg", "img2.png", "img1.gif", "notes.txt", "song.mp3"] {
            write(&dir.path().join(n), 10);
        }
        #[cfg(target_os = "macos")]
        {
            write(&dir.path().join("clip.mov"), 10);
            write(&dir.path().join("doc.pdf"), 10);
        }
        let p = pick(dir.path(), &Limits::LOCAL, later(), &|| false).unwrap();
        #[cfg(target_os = "macos")]
        assert_eq!(names(&p), ["img1.gif", "img2.png", "img10.jpg", "clip.mov"], "pictures, then a video");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(names(&p), ["img1.gif", "img2.png", "img10.jpg"]);
        assert!(!p.truncated && !p.cancelled && !p.raster_only);
    }

    #[test]
    fn hidden_files_folders_links_and_empty_files_are_never_picked() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(&d.join(".secret.png"), 10);
        write(&d.join("empty.png"), 0);
        fs::create_dir(d.join("sub.png")).unwrap();
        write(&d.join("real.png"), 10);
        std::os::unix::fs::symlink(d.join("real.png"), d.join("alias.png")).unwrap();
        let p = pick(d, &Limits::LOCAL, later(), &|| false).unwrap();
        assert_eq!(names(&p), ["real.png"]);
    }

    #[test]
    fn big_files_and_quick_look_kinds_stay_out_of_the_drive() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(&d.join("a.jpg"), 32);
        write(&d.join("b.pdf"), 32);
        let small = Limits { max_file_bytes: 16, ..Limits::DRIVE };
        assert!(pick(d, &small, later(), &|| false).unwrap().entries.is_empty(), "over the cap");
        let p = pick(d, &Limits::DRIVE, later(), &|| false).unwrap();
        assert_eq!(names(&p), ["a.jpg"], "no document on the drive");
        assert!(p.raster_only);
    }

    #[test]
    fn at_most_four_picks() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..9 {
            write(&dir.path().join(format!("p{i}.png")), 10);
        }
        assert_eq!(pick(dir.path(), &Limits::LOCAL, later(), &|| false).unwrap().entries.len(), MAX_PICKS);
    }

    #[test]
    fn the_scan_and_the_stats_stop_at_their_limits() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300 {
            write(&dir.path().join(format!("f{i:03}.txt")), 1);
        }
        let few = Limits { max_scanned: 100, ..Limits::LOCAL };
        let p = pick(dir.path(), &few, later(), &|| false).unwrap();
        assert_eq!(p.scanned, 100);
        assert!(p.truncated && p.entries.is_empty());
        // The two first candidates are too big; two stats only.
        let d2 = tempfile::tempdir().unwrap();
        write(&d2.path().join("a1.png"), 64);
        write(&d2.path().join("a2.png"), 64);
        write(&d2.path().join("a3.png"), 8);
        let limits = Limits { max_stats: 2, max_file_bytes: 16, ..Limits::LOCAL };
        let p = pick(d2.path(), &limits, later(), &|| false).unwrap();
        assert!(p.entries.is_empty() && p.truncated);
    }

    #[test]
    fn a_past_deadline_or_a_cancel_stops_at_once() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.png"), 10);
        let p = pick(dir.path(), &Limits::LOCAL, Instant::now() - Duration::from_millis(1), &|| false).unwrap();
        assert!(p.truncated && p.entries.is_empty());
        let p = pick(dir.path(), &Limits::LOCAL, later(), &|| true).unwrap();
        assert!(p.cancelled && p.entries.is_empty());
    }

    #[test]
    fn an_unreadable_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything: nothing to check then.
        if unsafe { libc::getuid() } != 0 {
            assert!(pick(&locked, &Limits::LOCAL, later(), &|| false).is_err());
        }
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn rules_follow_the_drive_mount_points_and_network_volumes() {
        let m = |on: &str, fstype: &str| Mount { from: "x".into(), on: on.into(), fstype: fstype.into(), browsable: true };
        let mounts = [m("/", "apfs"), m("/Users/demo/GitDrive", "macfuse"), m("/Volumes/share", "smbfs"), m("/mnt/remote", "fuse.sshfs"), m("/Volumes/USB", "msdos")];
        let drive = Some(Path::new("/Users/demo/GitDrive"));
        let rule = |p: &str, mp: bool| rules_for(Path::new(p), drive, &mounts, mp);
        assert_eq!(rule("/Users/demo/GitDrive/Fotos", false), Some(Limits::DRIVE));
        assert_eq!(rule("/Users/demo/GitDrive", true), None, "a mount point");
        assert_eq!(rule("/Users/demo/GitDrive2/x", false), Some(Limits::LOCAL), "a name that only starts like the drive");
        assert_eq!(rule("/Volumes/share/x", false), None, "network");
        assert_eq!(rule("/mnt/remote/x", false), None, "another FUSE");
        assert_eq!(rule("/Volumes/USB/x", false), Some(Limits::LOCAL));
        assert_eq!(rule("/Users/demo/Pictures", false), Some(Limits::LOCAL));
    }
}
