//! The sidebar and the Computer view: the user's bookmarks (saved in
//! `~/.config/git-drive/files.json`), the mounted volumes and the Trash.

use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// A folder the sidebar or the Computer view links to.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Place {
    pub label: String,
    pub path: String,
    /// Icon id for the frontend: drive, home, desktop, documents, downloads,
    /// pictures, movies, music or folder.
    pub icon: &'static str,
    /// A bookmark whose folder is gone (renamed, deleted, unmounted).
    pub missing: bool,
}

/// The standard folders of a home, with their icon ids; movies are "Movies"
/// on macOS and "Videos" on Linux.
const STANDARD: [(&str, &str); 7] = [
    ("Desktop", "desktop"),
    ("Documents", "documents"),
    ("Downloads", "downloads"),
    ("Pictures", "pictures"),
    ("Movies", "movies"),
    ("Videos", "movies"),
    ("Music", "music"),
];

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// How the sidebar shows `path`: the home folder by the user's name, the
/// drive by its own, standard folders with their icons.
pub fn place_for(path: &Path, home: &Path, drive: Option<&Path>) -> Place {
    let icon = if Some(path) == drive {
        "drive"
    } else if path == home {
        "home"
    } else {
        STANDARD
            .iter()
            .find(|(dir, _)| path == home.join(dir))
            .map_or("folder", |&(_, icon)| icon)
    };
    Place { label: name_of(path), path: path.to_string_lossy().into_owned(), icon, missing: !path.is_dir() }
}

/// Bookmarks before the user changes them: the drive, the home folder and
/// the folders of it everyone uses.
pub fn default_bookmarks(home: &Path, drive: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = drive.filter(|d| d.is_dir()).map(Path::to_path_buf).into_iter().collect();
    out.push(home.to_path_buf());
    out.extend(["Desktop", "Documents", "Downloads"].iter().map(|d| home.join(d)).filter(|p| p.is_dir()));
    out
}

/// The home's standard folders that exist, for the Computer view.
pub fn standard_dirs(home: &Path) -> Vec<Place> {
    STANDARD
        .iter()
        .map(|(dir, _)| home.join(dir))
        .filter(|p| p.is_dir())
        .map(|p| place_for(&p, home, None))
        .collect()
}

pub fn bookmarks_file() -> PathBuf {
    git_drive_state::paths::config_dir().join("files.json")
}

/// The saved bookmarks; `None` until the user changes them (the defaults apply).
pub fn load_bookmarks(file: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(file).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let list = value.get("bookmarks")?.as_array()?;
    Some(list.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
}

/// Saves the bookmarks in their order, keeping the file's other keys; atomic.
pub fn save_bookmarks(file: &Path, paths: &[String]) -> io::Result<()> {
    let mut prefs = std::fs::read_to_string(file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    prefs.insert("bookmarks".into(), Value::Array(paths.iter().cloned().map(Value::String).collect()));
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&prefs)? + "\n")?;
    std::fs::rename(&tmp, file)
}

/// A mounted file system, as the system lists them.
#[derive(Debug, Clone)]
pub struct Mount {
    /// The device or source it came from (`/dev/sda1`, `map auto_home`).
    pub from: String,
    pub on: String,
    pub fstype: String,
    /// macOS hides some volumes from the Finder (`MNT_DONTBROWSE`).
    pub browsable: bool,
}

/// File systems that are never disks the user put there.
const PSEUDO: [&str; 9] = ["proc", "sysfs", "tmpfs", "devtmpfs", "devfs", "overlay", "squashfs", "autofs", "ramfs"];

/// The volumes worth showing, as (mount point, kind): the system disk first,
/// then the drive, then the others by path. Kinds: system, drive, external.
/// macOS: `/`, browsable volumes in `/Volumes` and FUSE mounts in the home.
/// Linux: `/`, a separate `/home`, disks in `/run/media`, `/media` and
/// `/mnt`, and FUSE mounts in the home; a subvolume or bind mount of the
/// system partition is that same disk, not another one.
pub fn visible_mounts(mounts: &[Mount], home: &Path, drive: Option<&Path>, macos: bool) -> Vec<(String, &'static str)> {
    let root_from = mounts.iter().find(|m| m.on == "/").map(|m| m.from.as_str());
    let mut system = Vec::new();
    let mut drives = Vec::new();
    let mut others = Vec::new();
    for m in mounts {
        if PSEUDO.contains(&m.fstype.as_str()) {
            continue;
        }
        if !macos && m.on != "/" && Some(m.from.as_str()) == root_from {
            continue;
        }
        let on = Path::new(&m.on);
        let fuse_in_home = m.fstype.contains("fuse") && on.starts_with(home) && on != home;
        if m.on == "/" {
            system.push((m.on.clone(), "system"));
        } else if Some(on) == drive {
            drives.push((m.on.clone(), "drive"));
        } else if macos {
            if m.browsable && (on.starts_with("/Volumes") || fuse_in_home) {
                others.push((m.on.clone(), "external"));
            }
        } else if m.on == "/home" || ["/run/media", "/media", "/mnt"].iter().any(|r| on.starts_with(r) && on != Path::new(r)) || fuse_in_home {
            others.push((m.on.clone(), "external"));
        }
    }
    others.sort();
    others.dedup();
    system.into_iter().take(1).chain(drives.into_iter().take(1)).chain(others).collect()
}

/// `/proc/self/mounts`: device, mount point and type per line, with spaces
/// and other bytes octal-escaped (`\040`). Linux only (tested everywhere).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn parse_proc_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let from = unescape_octal(fields.next()?);
            let on = unescape_octal(fields.next()?);
            let fstype = fields.next()?.to_string();
            Some(Mount { from, on, fstype, browsable: true })
        })
        .collect()
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn unescape_octal(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 4 <= bytes.len() && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b)) {
            out.push((bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The mounted file systems.
#[cfg(target_os = "macos")]
pub fn read_mounts() -> Vec<Mount> {
    use std::ffi::CStr;
    use std::sync::Mutex;
    // getmntinfo's buffer is the process's own until the next call.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _guard = ONE_AT_A_TIME.lock();
    let mut buf: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo fills `buf` with `n` entries it owns.
    let n = unsafe { libc::getmntinfo(&mut buf, libc::MNT_NOWAIT) };
    if n <= 0 || buf.is_null() {
        return Vec::new();
    }
    // SAFETY: `n` valid entries, read before the next call (serialized above).
    let all = unsafe { std::slice::from_raw_parts(buf, n as usize) };
    let text = |chars: &[libc::c_char]| unsafe { CStr::from_ptr(chars.as_ptr()) }.to_string_lossy().into_owned();
    all.iter()
        .map(|s| Mount {
            from: text(&s.f_mntfromname),
            on: text(&s.f_mntonname),
            fstype: text(&s.f_fstypename),
            browsable: s.f_flags & libc::MNT_DONTBROWSE as u32 == 0,
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn read_mounts() -> Vec<Mount> {
    std::fs::read_to_string("/proc/self/mounts").map(|t| parse_proc_mounts(&t)).unwrap_or_default()
}

/// The system disk's name: macOS keeps a link to `/` in `/Volumes` named after it.
pub fn system_name() -> String {
    if cfg!(target_os = "macos") {
        let linked = std::fs::read_dir("/Volumes").ok().and_then(|entries| {
            entries.flatten().find(|e| std::fs::read_link(e.path()).is_ok_and(|t| t == Path::new("/"))).map(|e| e.file_name().to_string_lossy().into_owned())
        });
        linked.unwrap_or_else(|| "Macintosh HD".into())
    } else {
        t!("places.system_disk")
    }
}

/// A mounted volume for the sidebar and the Computer view.
#[derive(Debug, Clone, Serialize)]
pub struct Volume {
    pub name: String,
    pub path: String,
    pub kind: &'static str,
    pub total: u64,
    pub free: u64,
    /// The drive's repository against what its host accepts, as the daemon
    /// last measured it (`git_drive_state::status::RepoUsage`); `None` elsewhere.
    pub repo: Option<git_drive_state::status::RepoUsage>,
}

/// Bytes in decimal units, as the frontend writes them ("496 GB", "31.0 GB").
fn size_text(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    if v >= 100.0 || i == 0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// A volume's name: its mount point's, unless that is a file system's UUID
/// or serial (a disk with no label, mounted by udisks): then its size, as
/// GNOME Files names it ("496 GB Volume").
pub fn volume_name(path: &Path, total: u64) -> String {
    let name = name_of(path);
    let hex_groups = |lens: &[usize]| {
        let parts: Vec<&str> = name.split('-').collect();
        parts.len() == lens.len() && parts.iter().zip(lens).all(|(p, &n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
    };
    if !(hex_groups(&[8, 4, 4, 4, 12]) || hex_groups(&[4, 4])) {
        return name;
    }
    if total == 0 {
        t!("places.volume")
    } else {
        t!("places.sized_volume", size = size_text(total))
    }
}

/// The visible volumes with their space; `measure` gives (total, free) of a mount point.
pub fn volumes(home: &Path, drive: Option<&Path>, measure: impl Fn(&Path) -> Option<(u64, u64)>) -> Vec<Volume> {
    visible_mounts(&read_mounts(), home, drive, cfg!(target_os = "macos"))
        .into_iter()
        .map(|(path, kind)| {
            let p = Path::new(&path);
            let (total, free) = measure(p).unwrap_or((0, 0));
            let name = if kind == "system" { system_name() } else { volume_name(p, total) };
            Volume { name, path, kind, total, free, repo: None }
        })
        .collect()
}

/// The Trash: `~/.Trash` on macOS; on Linux the freedesktop one
/// (`$XDG_DATA_HOME/Trash`, with `files/` and `info/`).
pub fn trash_root(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        return home.join(".Trash");
    }
    let data = std::env::var_os("XDG_DATA_HOME").map_or_else(|| home.join(".local/share"), PathBuf::from);
    data.join("Trash")
}

/// The folder whose contents the Trash shows.
pub fn trash_files(root: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        root.to_path_buf()
    } else {
        root.join("files")
    }
}

fn percent_decode(s: &str) -> PathBuf {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

/// Puts a trashed item (freedesktop Trash) back where it was deleted from,
/// remaking the folder it left; refuses when something is there now.
pub fn put_back(root: &Path, item: &Path) -> Result<PathBuf, String> {
    let name = item.file_name().ok_or_else(|| t!("places.not_trash_item"))?.to_string_lossy().into_owned();
    let info = root.join("info").join(format!("{name}.trashinfo"));
    let text = std::fs::read_to_string(&info).map_err(|_| t!("places.no_record", name = name))?;
    let original = text
        .lines()
        .find_map(|l| l.strip_prefix("Path="))
        .map(percent_decode)
        .ok_or_else(|| t!("places.no_record", name = name))?;
    if std::fs::symlink_metadata(&original).is_ok() {
        let parent = original.parent().map_or_else(String::new, |p| p.display().to_string());
        return Err(t!("places.exists_in", name = name_of(&original), folder = parent));
    }
    if let Some(parent) = original.parent() {
        std::fs::create_dir_all(parent).map_err(|e| t!("places.remake_failed", path = parent.display(), error = crate::i18n::io_text(&e)))?;
    }
    crate::ops::rename_noreplace(item, &original).map_err(|e| match e.kind() {
        ErrorKind::AlreadyExists => t!("places.exists_there", name = name_of(&original)),
        _ => t!("places.put_back_failed", name = name, error = crate::i18n::io_text(&e)),
    })?;
    let _ = std::fs::remove_file(&info);
    Ok(original)
}

/// Drops the records (freedesktop `info/<name>.trashinfo`) of items deleted
/// from the Trash's `files/`; paths elsewhere are left alone.
pub fn forget_trash_records(root: &Path, deleted: &[PathBuf]) {
    let files = root.join("files");
    for path in deleted {
        if path.parent() == Some(files.as_path()) {
            if let Some(name) = path.file_name() {
                let _ = std::fs::remove_file(root.join("info").join(format!("{}.trashinfo", name.to_string_lossy())));
            }
        }
    }
}

/// Deletes everything in the Trash for good, entry by entry (a cancel
/// stops between two); returns how many items went.
pub fn empty_trash(root: &Path, task: &crate::tasks::Task) -> Result<usize, String> {
    let files = if root.join("files").is_dir() { root.join("files") } else { root.to_path_buf() };
    let items: Vec<PathBuf> = std::fs::read_dir(&files).map_err(|e| t!("places.trash_open_failed", error = crate::i18n::io_text(&e)))?.flatten().map(|e| e.path()).collect();
    let (_, entries) = crate::tasks::measure(&items, task).map_err(|_| crate::tasks::CANCELLED.to_string())?;
    task.set_totals(0, entries, items.len() as u64);
    let mut removed = 0;
    for path in &items {
        let gone = crate::ops::remove_counted(path, task);
        if gone.is_ok() {
            forget_trash_records(root, std::slice::from_ref(path));
        }
        gone.map_err(|e| task.explain(e, |e| t!("ops.delete_failed", name = name_of(path), error = crate::i18n::io_text(&e))))?;
        task.item_done();
        removed += 1;
    }
    if let Ok(infos) = std::fs::read_dir(root.join("info")) {
        for info in infos.flatten() {
            let _ = std::fs::remove_file(info.path());
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn mount(on: &str, fstype: &str, browsable: bool) -> Mount {
        Mount { from: format!("dev:{on}"), on: on.into(), fstype: fstype.into(), browsable }
    }

    #[test]
    fn default_bookmarks_name_the_home_folder_after_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("julian");
        for d in ["Desktop", "Documents"] {
            fs::create_dir_all(home.join(d)).unwrap();
        }
        let drive = dir.path().join("GitDrive");
        fs::create_dir(&drive).unwrap();
        let paths = default_bookmarks(&home, Some(&drive));
        assert_eq!(paths, vec![drive.clone(), home.clone(), home.join("Desktop"), home.join("Documents")], "Downloads is missing here");
        let places: Vec<Place> = paths.iter().map(|p| place_for(p, &home, Some(&drive))).collect();
        let labels: Vec<(&str, &str)> = places.iter().map(|p| (p.label.as_str(), p.icon)).collect();
        assert_eq!(labels, [("GitDrive", "drive"), ("julian", "home"), ("Desktop", "desktop"), ("Documents", "documents")]);
        assert!(places.iter().all(|p| !p.missing));
        let gone = place_for(&home.join("Old"), &home, None);
        assert_eq!((gone.label.as_str(), gone.icon, gone.missing), ("Old", "folder", true));
    }

    #[test]
    fn saved_bookmarks_keep_their_order_and_replace_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("files.json");
        assert_eq!(load_bookmarks(&file), None, "never saved: the defaults apply");
        let order = vec!["/b".to_string(), "/a".to_string()];
        save_bookmarks(&file, &order).unwrap();
        assert_eq!(load_bookmarks(&file), Some(order));
        save_bookmarks(&file, &[]).unwrap();
        assert_eq!(load_bookmarks(&file), Some(vec![]), "an emptied list stays empty");
    }

    #[test]
    fn macos_shows_the_system_disk_the_drive_and_browsable_volumes() {
        let home = Path::new("/Users/u");
        let drive = Path::new("/Users/u/GitDrive");
        let mounts = [
            mount("/", "apfs", true),
            mount("/System/Volumes/Data", "apfs", false),
            mount("/System/Volumes/VM", "apfs", false),
            mount("/dev", "devfs", true),
            mount("/Volumes/USB STICK", "msdos", true),
            mount("/Volumes/Recovery", "apfs", false),
            mount("/Users/u/GitDrive", "macfuse", true),
            mount("/System/Volumes/Data/home", "autofs", true),
        ];
        let got = visible_mounts(&mounts, home, Some(drive), true);
        let got: Vec<(&str, &str)> = got.iter().map(|(p, k)| (p.as_str(), *k)).collect();
        assert_eq!(got, [("/", "system"), ("/Users/u/GitDrive", "drive"), ("/Volumes/USB STICK", "external")]);
    }

    #[test]
    fn linux_shows_the_system_disk_the_drive_and_removable_or_mounted_disks() {
        let home = Path::new("/home/u");
        let drive = Path::new("/home/u/GitDrive");
        let mounts = [
            mount("/", "btrfs", true),
            mount("/home", "btrfs", true),
            mount("/proc", "proc", true),
            mount("/tmp", "tmpfs", true),
            mount("/boot", "vfat", true),
            mount("/run/media/u/STICK", "vfat", true),
            mount("/mnt/data", "ext4", true),
            mount("/snap/core/123", "squashfs", true),
            mount("/run/user/1000/doc", "fuse.portal", true),
            mount("/home/u/GitDrive", "fuse", true),
        ];
        let got = visible_mounts(&mounts, home, Some(drive), false);
        let got: Vec<(&str, &str)> = got.iter().map(|(p, k)| (p.as_str(), *k)).collect();
        assert_eq!(
            got,
            [("/", "system"), ("/home/u/GitDrive", "drive"), ("/home", "external"), ("/mnt/data", "external"), ("/run/media/u/STICK", "external")]
        );
    }

    #[test]
    fn a_subvolume_of_the_system_partition_is_not_another_disk() {
        let home = Path::new("/home/u");
        let on_root = |on: &str| Mount { from: "/dev/nvme0n1p2".into(), on: on.into(), fstype: "btrfs".into(), browsable: true };
        let mounts = [on_root("/"), on_root("/home"), on_root("/mnt/snapshots"), mount("/mnt/data", "ext4", true)];
        let got = visible_mounts(&mounts, home, None, false);
        let got: Vec<&str> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(got, ["/", "/mnt/data"]);
    }

    #[test]
    fn a_disk_without_a_label_is_named_by_its_size() {
        assert_eq!(volume_name(Path::new("/run/media/u/d4491749-efa8-47b8-876e-21d862b1dc7f"), 496_309_014_528), "496 GB Volume");
        assert_eq!(volume_name(Path::new("/run/media/u/5A1C-2B3D"), 31_000_000_000), "31.0 GB Volume", "a FAT serial is no name either");
        assert_eq!(volume_name(Path::new("/run/media/u/DADOS"), 499_031_994_368), "DADOS");
        assert_eq!(volume_name(Path::new("/run/media/u/5A1C-2B3D"), 0), "Volume", "size unknown");
    }

    #[test]
    fn proc_mounts_unescape_spaces_in_mount_points() {
        let text = "/dev/sda1 / ext4 rw 0 0\n/dev/sdb1 /run/media/u/My\\040Stick vfat rw 0 0\nproc /proc proc rw 0 0\n";
        let mounts = parse_proc_mounts(text);
        assert_eq!(mounts.len(), 3);
        assert_eq!((mounts[1].on.as_str(), mounts[1].fstype.as_str()), ("/run/media/u/My Stick", "vfat"));
        assert_eq!(mounts[1].from, "/dev/sdb1");
        // A backslash too close to the end is kept as it is.
        assert_eq!(unescape_octal("/mnt/a\\04"), "/mnt/a\\04");
    }

    fn trashed(root: &Path, name: &str, original: &str) -> std::path::PathBuf {
        fs::create_dir_all(root.join("files")).unwrap();
        fs::create_dir_all(root.join("info")).unwrap();
        let item = root.join("files").join(name);
        fs::write(&item, "kept").unwrap();
        let info = format!("[Trash Info]\nPath={original}\nDeletionDate=2026-10-16T05:00:00\n");
        fs::write(root.join("info").join(format!("{name}.trashinfo")), info).unwrap();
        item
    }

    #[test]
    fn an_item_goes_back_where_it_was_deleted_from() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let original = dir.path().join("old folder/report.pdf");
        let encoded = original.to_string_lossy().replace(' ', "%20");
        let item = trashed(&trash, "report.pdf", &encoded);
        let back = put_back(&trash, &item).unwrap();
        assert_eq!(back, original);
        assert_eq!(fs::read_to_string(&original).unwrap(), "kept", "the folder it left is made again");
        assert!(!item.exists());
        assert!(!trash.join("info/report.pdf.trashinfo").exists(), "its record goes with it");
    }

    #[test]
    fn putting_back_never_replaces_what_is_there_now() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let original = dir.path().join("notes.txt");
        fs::write(&original, "newer").unwrap();
        let item = trashed(&trash, "notes.txt", &original.to_string_lossy());
        let err = put_back(&trash, &item).unwrap_err();
        assert!(err.contains("already"), "{err}");
        assert_eq!(fs::read_to_string(&original).unwrap(), "newer");
        assert!(item.exists(), "it stays in the Trash");
    }

    #[test]
    fn deleting_from_the_trash_drops_the_records_of_what_went() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        let gone = trashed(&trash, "a.txt", "/x/a.txt");
        trashed(&trash, "b.txt", "/x/b.txt");
        fs::remove_file(&gone).unwrap();
        let elsewhere = dir.path().join("b.txt");
        forget_trash_records(&trash, &[gone, elsewhere]);
        assert!(!trash.join("info/a.txt.trashinfo").exists());
        assert!(trash.join("info/b.txt.trashinfo").exists(), "a path outside the Trash leaves its namesake alone");
    }

    #[test]
    fn emptying_the_trash_removes_items_and_their_records() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("Trash");
        trashed(&trash, "a.txt", "/x/a.txt");
        fs::create_dir_all(trash.join("files/folder/inner")).unwrap();
        assert_eq!(empty_trash(&trash, &crate::tasks::Task::default()).unwrap(), 2);
        assert_eq!(fs::read_dir(trash.join("files")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(trash.join("info")).unwrap().count(), 0);
    }
}


