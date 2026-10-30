//! Properties of a file or folder (Finder's Get Info): what `lstat` says of
//! it in words the window shows (kind, sizes, dates, owner and group by
//! name, permissions, a link's target, extended attributes), and the changes
//! the dialog makes: permissions (one item, several, or a folder with
//! everything in it) and, on macOS, the quarantine flag of a download.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::tasks::Task;

/// The quarantine flag macOS puts on downloads (Gatekeeper asks before opening them).
pub const QUARANTINE: &str = "com.apple.quarantine";

#[derive(Debug, Serialize)]
pub struct Xattr {
    pub name: String,
    pub size: usize,
}

#[derive(Debug, Serialize)]
pub struct Info {
    pub path: String,
    pub name: String,
    /// "file", "dir", "symlink" or "other" (a socket, a pipe, a device).
    pub kind: &'static str,
    /// Bytes of a file (or of a link's path); folders get theirs from `folder_size`.
    pub size: u64,
    /// What it takes on disk (allocated blocks).
    pub on_disk: u64,
    pub created: Option<i64>,
    pub modified: Option<i64>,
    pub accessed: Option<i64>,
    /// When its inode last changed (permissions, owner, links).
    pub changed: Option<i64>,
    /// Permission bits with the special ones (setuid, setgid, sticky).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub owner: Option<String>,
    pub group: Option<String>,
    /// The user running the app owns it (or is root): its permissions can change.
    pub mine: bool,
    pub links: u64,
    pub inode: u64,
    pub device: u64,
    pub link_target: Option<String>,
    /// A link whose target is not there.
    pub link_broken: bool,
    pub xattrs: Vec<Xattr>,
}

fn secs(t: io::Result<std::time::SystemTime>) -> Option<i64> {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64)
}

/// A user's name by id; None when the system has no entry for it.
pub fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: getpwuid_r fills `pwd` with pointers into `buf`, both alive here.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() {
        return None;
    }
    // SAFETY: pw_name is a NUL-terminated string in `buf`.
    Some(unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) }.to_string_lossy().into_owned())
}

/// A group's name by id.
pub fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut out: *mut libc::group = std::ptr::null_mut();
    // SAFETY: getgrgid_r fills `grp` with pointers into `buf`, both alive here.
    let rc = unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() {
        return None;
    }
    // SAFETY: gr_name is a NUL-terminated string in `buf`.
    Some(unsafe { std::ffi::CStr::from_ptr(grp.gr_name) }.to_string_lossy().into_owned())
}

/// The extended attributes of `path` itself (never a link's target), with their sizes.
pub fn xattrs(path: &Path) -> Vec<Xattr> {
    let Ok(names) = xattr::list(path) else { return Vec::new() };
    let mut out: Vec<Xattr> = names
        .map(|n| {
            let size = xattr::get(path, &n).ok().flatten().map_or(0, |v| v.len());
            Xattr { name: n.to_string_lossy().into_owned(), size }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// What `lstat` says of `path`, in the dialog's terms.
pub fn info(path: &Path) -> io::Result<Info> {
    let meta = fs::symlink_metadata(path)?;
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        "symlink"
    } else if ft.is_dir() {
        "dir"
    } else if ft.is_file() {
        "file"
    } else {
        "other"
    };
    let link_target = ft.is_symlink().then(|| fs::read_link(path).ok()).flatten();
    // SAFETY: getuid never fails.
    let me = unsafe { libc::getuid() };
    Ok(Info {
        path: path.display().to_string(),
        name: path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
        kind,
        size: if ft.is_dir() { 0 } else { meta.len() },
        on_disk: meta.blocks() * 512,
        created: secs(meta.created()),
        modified: secs(meta.modified()),
        accessed: secs(meta.accessed()),
        changed: Some(meta.ctime()),
        mode: meta.mode() & 0o7777,
        uid: meta.uid(),
        gid: meta.gid(),
        owner: user_name(meta.uid()),
        group: group_name(meta.gid()),
        mine: me == 0 || me == meta.uid(),
        links: meta.nlink(),
        inode: meta.ino(),
        device: meta.dev(),
        link_broken: ft.is_symlink() && fs::metadata(path).is_err(),
        link_target: link_target.map(|t| t.display().to_string()),
        xattrs: xattrs(path),
    })
}

/// A change of permissions over several items: bits to set and bits to
/// clear; the others stay as each item has them (several items may differ).
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
pub struct ModeChange {
    pub set: u32,
    pub clear: u32,
}

impl ModeChange {
    pub fn apply(&self, mode: u32) -> u32 {
        ((mode & !self.clear) | self.set) & 0o7777
    }
}

/// Applies `change` to `paths` (links are left alone: their own mode means
/// nothing); with `recursive`, also to everything inside the folders among
/// them. Counts each item on `task`; stops at the next one once cancelled.
pub fn set_mode(paths: &[PathBuf], change: ModeChange, recursive: bool, task: &Task) -> Result<usize, String> {
    let mut stack: Vec<PathBuf> = paths.iter().rev().cloned().collect();
    let mut changed = 0;
    task.set_totals(0, 0, paths.len() as u64);
    while let Some(path) = stack.pop() {
        task.check().map_err(|e| task.explain(e, |e| e.to_string()))?;
        task.on(&path);
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let meta = fs::symlink_metadata(&path).map_err(|e| t!("props.item_error", name = name, error = crate::i18n::io_text(&e)))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        let next = change.apply(meta.mode());
        if next != meta.mode() & 0o7777 {
            fs::set_permissions(&path, fs::Permissions::from_mode(next)).map_err(|e| t!("props.change_failed", name = name, error = crate::i18n::io_text(&e)))?;
            changed += 1;
        }
        if recursive && meta.is_dir() {
            // Read after the change: a folder made readable can now be listed.
            let mut inside: Vec<PathBuf> = fs::read_dir(&path).map_err(|e| t!("props.open_failed", name = name, error = crate::i18n::io_text(&e)))?.flatten().map(|e| e.path()).collect();
            inside.sort();
            stack.extend(inside.into_iter().rev());
        }
        if paths.contains(&path) {
            task.item_done();
        }
        task.file_done();
    }
    Ok(changed)
}

/// The error of a missing extended attribute.
#[cfg(target_os = "macos")]
const NO_ATTR: i32 = libc::ENOATTR;
#[cfg(not(target_os = "macos"))]
const NO_ATTR: i32 = libc::ENODATA;

/// Removes the quarantine flag (macOS asks no more before opening the download).
pub fn remove_quarantine(path: &Path) -> Result<(), String> {
    match xattr::remove(path, QUARANTINE) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(NO_ATTR) => Ok(()),
        Err(e) => Err(t!("props.quarantine_failed", error = crate::i18n::io_text(&e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_names_the_owner_and_reads_a_link_without_following_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.md");
        fs::write(&file, "hello").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let i = info(&file).unwrap();
        assert_eq!((i.kind, i.size, i.mode), ("file", 5, 0o640));
        assert!(i.mine);
        assert_eq!(i.owner, user_name(unsafe { libc::getuid() }));
        assert!(i.owner.is_some() && i.group.is_some());
        assert!(i.modified.is_some() && i.created.is_some());

        std::os::unix::fs::symlink("nowhere", dir.path().join("dangling")).unwrap();
        let l = info(&dir.path().join("dangling")).unwrap();
        assert_eq!((l.kind, l.link_target.as_deref(), l.link_broken), ("symlink", Some("nowhere"), true));
        assert_eq!(info(dir.path()).unwrap().kind, "dir");
    }

    #[test]
    fn extended_attributes_are_listed_with_their_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a");
        fs::write(&file, "x").unwrap();
        // macOS takes any name; Linux wants the user namespace (and a file system with xattrs).
        if xattr::set(&file, "user.test.note", b"four").is_err() {
            return;
        }
        let names: Vec<(String, usize)> = info(&file).unwrap().xattrs.into_iter().map(|x| (x.name, x.size)).collect();
        assert!(names.contains(&("user.test.note".to_string(), 4)), "{names:?}");
    }

    #[test]
    fn a_change_sets_and_clears_bits_and_keeps_the_rest() {
        let c = ModeChange { set: 0o004, clear: 0o020 };
        assert_eq!(c.apply(0o660), 0o644);
        assert_eq!(c.apply(0o755), 0o755, "already so");
        assert_eq!(ModeChange { set: 0o1000, clear: 0 }.apply(0o777), 0o1777, "sticky");
    }

    #[test]
    fn permissions_change_through_a_folder_and_skip_links() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("top");
        fs::create_dir_all(top.join("sub")).unwrap();
        fs::write(top.join("a.txt"), "a").unwrap();
        fs::write(top.join("sub/b.txt"), "b").unwrap();
        std::os::unix::fs::symlink("a.txt", top.join("link")).unwrap();
        for p in ["a.txt", "sub/b.txt"] {
            fs::set_permissions(top.join(p), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mode = |p: &str| fs::metadata(top.join(p)).unwrap().mode() & 0o777;
        let change = ModeChange { set: 0o044, clear: 0 };
        let task = Task::default();
        // Not recursive: the folder alone.
        set_mode(std::slice::from_ref(&top), change, false, &task).unwrap();
        assert_eq!(mode("a.txt"), 0o600);
        set_mode(std::slice::from_ref(&top), change, true, &task).unwrap();
        assert_eq!((mode("a.txt"), mode("sub/b.txt")), (0o644, 0o644));
        let cancelled = Task::default();
        cancelled.cancel();
        assert!(set_mode(&[top.join("a.txt")], ModeChange { set: 0, clear: 0o044 }, false, &cancelled).is_err());
        assert_eq!(mode("a.txt"), 0o644, "a cancel changes nothing more");
    }
}
