//! The context menu's file operations: new folder, rename, paste (copy or
//! move) and delete (to the Trash, or permanently once the user confirmed).
//! Nothing is ever overwritten: a rename to a name already taken is refused,
//! a paste gives it a free variant, as Finder's "Keep Both" does ("name 2"),
//! and a copy into its own folder is a duplicate ("name copy", "name copy 2").
//! The long ones report their progress to a `Task` and stop once it is
//! cancelled, removing what they had half done.

use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::tasks::{measure, Task, CANCELLED};

/// How much a copy moves between two looks at its task (progress, cancel).
const COPY_CHUNK: usize = 4 << 20;

/// Longest file name, in bytes, that macOS and Linux file systems accept.
const NAME_MAX: usize = 255;

/// A name typed by the user, trimmed; refused when no file system would take it.
pub fn check_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(t!("ops.name_empty"));
    }
    if name == "." || name == ".." {
        return Err(t!("ops.name_reserved", name = name));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(t!("ops.name_slash"));
    }
    if name.len() > NAME_MAX {
        return Err(t!("ops.name_too_long"));
    }
    Ok(name)
}

/// Creates the folder `name` in `parent`; refuses a name already taken.
pub fn new_folder(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = check_name(name)?;
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => Ok(path),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(t!("ops.folder_exists", name = name)),
        Err(e) => Err(t!("ops.create_failed", name = name, error = crate::i18n::io_text(&e))),
    }
}

/// "report.pdf" -> ("report", ".pdf"). Folders and dotfiles (".env") keep their whole name.
fn split_ext(name: &str, is_dir: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The `n`-th free-name candidate (from 1): a duplicate ("x copy", "x copy 2")
/// or a second item of the same name ("x 2", "x 3").
fn variant(name: &str, is_dir: bool, duplicate: bool, n: u32) -> String {
    let (stem, ext) = split_ext(name, is_dir);
    // "copy" in the window's language, as Finder names a duplicate.
    let copy = t!("ops.copy_suffix");
    match (duplicate, n) {
        (true, 1) => format!("{stem} {copy}{ext}"),
        (true, n) => format!("{stem} {copy} {n}{ext}"),
        (false, n) => format!("{stem} {}{ext}", n + 1),
    }
}

/// Whether anything (a broken link included) is at `path`.
fn taken(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Where `name` lands in `dir`: the name itself when free (never for a
/// duplicate), else the first free variant.
pub fn free_target(dir: &Path, name: &str, is_dir: bool, duplicate: bool) -> PathBuf {
    let plain = dir.join(name);
    if !duplicate && !taken(&plain) {
        return plain;
    }
    (1..)
        .map(|n| dir.join(variant(name, is_dir, duplicate, n)))
        .find(|p| !taken(p))
        .expect("an unbounded range always finds a free name")
}

/// Copies `src` (a file, a link as a link, or a folder with everything in it)
/// to `dst`, counting each entry and the bytes of each file on `task`.
fn copy_tree(src: &Path, dst: &Path, task: &Task) -> io::Result<()> {
    task.check()?;
    task.on(src);
    let meta = fs::symlink_metadata(src)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
    } else if kind.is_dir() {
        fs::create_dir(dst)?;
        fill_dir(src, dst, &meta, task)?;
    } else if kind.is_file() {
        copy_file(src, dst, &meta, task)?;
    } else {
        return Err(io::Error::other(t!("ops.not_file_folder_link", path = src.display())));
    }
    task.file_done();
    Ok(())
}

/// Copies the contents of the folder `src` into the new folder `dst`.
fn fill_dir(src: &Path, dst: &Path, meta: &fs::Metadata, task: &Task) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        copy_tree(&entry.path(), &dst.join(entry.file_name()), task)?;
    }
    // After the contents: a read-only source folder must not block filling its copy.
    fs::set_permissions(dst, meta.permissions())
}

/// Copies the file `src` to the new file `dst` (never over one), keeping its
/// permissions and modification time. On APFS the copy is a clone (instant,
/// no space taken until one of them changes), as Finder's; elsewhere it goes
/// by chunks, in the kernel on Linux, so a cancel stops it within one.
fn copy_file(src: &Path, dst: &Path, meta: &fs::Metadata, task: &Task) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    match clone_file(src, dst) {
        Ok(()) => {
            task.add_bytes(meta.len());
            return Ok(());
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(e),
        // Another volume, or one without clones: copy the bytes.
        Err(_) => {}
    }
    let mut from = fs::File::open(src)?;
    let mut to = fs::OpenOptions::new().write(true).create_new(true).open(dst)?;
    #[cfg(target_os = "linux")]
    let in_kernel = kernel_copy(&from, &to, task)?;
    #[cfg(not(target_os = "linux"))]
    let in_kernel = false;
    if !in_kernel {
        let mut buf = vec![0u8; COPY_CHUNK];
        loop {
            task.check()?;
            let n = match from.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            to.write_all(&buf[..n])?;
            task.add_bytes(n as u64);
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // Extended attributes, ACLs, flags and times, as Finder keeps them.
        // SAFETY: two open descriptors and no state.
        unsafe { libc::fcopyfile(from.as_raw_fd(), to.as_raw_fd(), std::ptr::null_mut(), libc::COPYFILE_METADATA | libc::COPYFILE_STAT) };
    }
    to.set_permissions(meta.permissions())?;
    if let Ok(modified) = meta.modified() {
        let _ = to.set_modified(modified);
    }
    Ok(())
}

/// clonefile(2): the copy shares the original's blocks until one changes.
#[cfg(target_os = "macos")]
fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    let (src_c, dst_c) = (c_path(src)?, c_path(dst)?);
    // SAFETY: two NUL-terminated paths that outlive the call.
    match unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// copy_file_range(2) by chunks; false when this file system can't (then
/// nothing was copied and the bytes go through user space).
#[cfg(target_os = "linux")]
fn kernel_copy(from: &fs::File, to: &fs::File, task: &Task) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut copied_any = false;
    loop {
        task.check()?;
        // SAFETY: two open descriptors; null offsets use (and move) the files' own.
        let n = unsafe { libc::copy_file_range(from.as_raw_fd(), std::ptr::null_mut(), to.as_raw_fd(), std::ptr::null_mut(), COPY_CHUNK, 0) };
        if n < 0 {
            let e = io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EXDEV | libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP | libc::EPERM) if !copied_any => return Ok(false),
                _ => return Err(e),
            }
        }
        if n == 0 {
            return Ok(true);
        }
        copied_any = true;
        task.add_bytes(n as u64);
    }
}

fn c_path(p: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).map_err(io::Error::other)
}

/// `copy_tree` to a target that did not exist: on failure (a cancel
/// included), what this copy created is removed, so no half copy stays
/// behind. A target that appeared meanwhile (its creation fails with
/// `AlreadyExists`) is someone else's and stays as it is.
fn copy_fresh(src: &Path, dst: &Path, task: &Task) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() {
        task.check()?;
        task.on(src);
        fs::create_dir(dst)?;
        let filled = fill_dir(src, dst, &meta, task);
        if filled.is_err() {
            let _ = fs::remove_dir_all(dst);
        } else {
            task.file_done();
        }
        return filled;
    }
    let result = copy_tree(src, dst, task);
    if matches!(&result, Err(e) if e.kind() != ErrorKind::AlreadyExists) {
        let _ = fs::remove_file(dst);
    }
    result
}

/// Removes a file, a link (never what it points to) or a whole folder.
fn remove(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// `remove`, one entry at a time, counting each on `task` and stopping
/// between two once it is cancelled (what went before is gone).
pub(crate) fn remove_counted(path: &Path, task: &Task) -> io::Result<()> {
    task.check()?;
    task.on(path);
    if fs::symlink_metadata(path)?.is_dir() {
        let children: Vec<PathBuf> = fs::read_dir(path)?.flatten().map(|e| e.path()).collect();
        for child in children {
            remove_counted(&child, task)?;
        }
        fs::remove_dir(path)?;
    } else {
        fs::remove_file(path)?;
    }
    task.file_done();
    Ok(())
}

fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Copies (or moves, when `cut`) each of `sources` into `dest`, returning
/// where each one landed. Moving within its own folder does nothing; moving
/// to another volume copies, then removes the original once the copy is whole.
/// A cancel stops at the next chunk: the items already done stay, the one
/// under way is removed (a move leaves its original where it was).
pub fn paste(sources: &[PathBuf], dest: &Path, cut: bool, task: &Task) -> Result<Vec<PathBuf>, String> {
    let dest_real = fs::canonicalize(dest).map_err(|e| t!("ops.cant_paste_into", path = dest.display(), error = crate::i18n::io_text(&e)))?;
    // A copy measures everything first; a move only what crosses volumes
    // (a rename is instant), and that once it finds out.
    let (bytes, files) = if cut { (0, 0) } else { measure(sources, task).map_err(|_| CANCELLED.to_string())? };
    task.set_totals(bytes, files, sources.len() as u64);
    let mut landed = Vec::with_capacity(sources.len());
    for src in sources {
        let name = display_name(src);
        let meta = fs::symlink_metadata(src).map_err(|_| t!("ops.gone", name = name))?;
        let Some(file_name) = src.file_name() else {
            return Err(t!("ops.cant_paste", path = src.display()));
        };
        let is_dir = meta.is_dir();
        let src_real = fs::canonicalize(src).map_err(|e| t!("ops.cant_read", name = name, error = crate::i18n::io_text(&e)))?;
        if is_dir && dest_real.starts_with(&src_real) {
            return Err(if cut { t!("ops.move_into_itself", name = name) } else { t!("ops.copy_into_itself", name = name) });
        }
        let same_folder = src.parent().and_then(|p| fs::canonicalize(p).ok()).as_deref() == Some(&dest_real);
        if task.is_cancelled() {
            return Err(CANCELLED.into());
        }
        task.on(src);
        if cut && same_folder {
            task.item_done();
            landed.push(src.clone());
            continue;
        }
        let target = free_target(dest, &file_name.to_string_lossy(), is_dir, !cut && same_folder);
        if cut {
            move_to(src, &target, task).map_err(|e| task.explain(e, |e| t!("ops.move_failed", name = name, error = crate::i18n::io_text(&e))))?;
        } else {
            copy_fresh(src, &target, task).map_err(|e| task.explain(e, |e| t!("ops.copy_failed", name = name, error = crate::i18n::io_text(&e))))?;
        }
        task.item_done();
        landed.push(target);
    }
    Ok(landed)
}

/// A rename, or across volumes a whole copy followed by removing the original.
fn move_to(src: &Path, target: &Path, task: &Task) -> io::Result<()> {
    match rename_noreplace(src, target) {
        Err(e) if e.kind() == ErrorKind::CrossesDevices => {
            let (bytes, files) = measure(&[src.to_path_buf()], task)?;
            task.add_totals(bytes, files);
            copy_fresh(src, target, task)?;
            remove(src).map_err(|e| io::Error::other(t!("ops.original_not_removed", error = crate::i18n::io_text(&e))))
        }
        other => other,
    }
}

/// rename(2) that fails with `AlreadyExists` when something is at `to`, in one
/// step: checking first leaves a moment in which a new item there would be
/// replaced. A file system without the flag (some network and FUSE ones)
/// gets the check instead.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let (from_c, to_c) = (c_path(from)?, c_path(to)?);
    // SAFETY: two NUL-terminated paths that outlive the call.
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::renamex_np(from_c.as_ptr(), to_c.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    let rc = unsafe { libc::renameat2(libc::AT_FDCWD, from_c.as_ptr(), libc::AT_FDCWD, to_c.as_ptr(), libc::RENAME_NOREPLACE) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    let unsupported = err.raw_os_error().is_some_and(|c| [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS].contains(&c));
    match unsupported {
        true if taken(to) => Err(ErrorKind::AlreadyExists.into()),
        true => fs::rename(from, to),
        false => Err(err),
    }
}

/// Renames `path` within its folder, returning the new path. A name already
/// taken is refused, never replaced.
pub fn rename(path: &Path, name: &str, keep: &[PathBuf]) -> Result<PathBuf, String> {
    let name = check_name(name)?;
    let old = display_name(path);
    let parent = match path.parent() {
        Some(parent) if !protected(path, keep) => parent,
        _ => return Err(t!("ops.cant_rename_here", name = old)),
    };
    let target = parent.join(name);
    if target == path {
        return Ok(target);
    }
    let meta = fs::symlink_metadata(path).map_err(|_| t!("ops.gone", name = old))?;
    let result = if case_change(&meta, &old, name, &target) { fs::rename(path, &target) } else { rename_noreplace(path, &target) };
    match result {
        Ok(()) => Ok(target),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Err(t!("ops.name_taken", name = name)),
        Err(e) => Err(t!("ops.rename_failed", name = old, error = crate::i18n::io_text(&e))),
    }
}

/// Whether `target` is the item itself under a name that differs only in case,
/// on a case-insensitive volume. APFS takes that rename with `RENAME_EXCL`, but
/// Linux refuses it on vfat, exFAT or a casefolded ext4, and so does the
/// fallback check. Two hard links of one file are two names, not one.
fn case_change(meta: &fs::Metadata, old: &str, name: &str, target: &Path) -> bool {
    old.to_lowercase() == name.to_lowercase()
        && fs::symlink_metadata(target).is_ok_and(|t| t.dev() == meta.dev() && t.ino() == meta.ino())
}

/// A path that must never be deleted from here: a root, a home folder or a
/// place the sidebar lists.
pub fn protected(path: &Path, keep: &[PathBuf]) -> bool {
    path.parent().is_none() || path.parent() == Some(Path::new("/")) || keep.iter().any(|k| k == path)
}

/// Outcome of a move to the Trash.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "outcome")]
pub enum Trashed {
    Done,
    /// `remaining` were not touched (the ones before them went to the Trash);
    /// deleting them permanently is the only way, after the user confirms.
    NoTrash { reason: String, remaining: Vec<PathBuf> },
}

/// Moves `paths` to the Trash, one at a time (a cancel stops between two).
/// On Linux the drive (`drive`) has no Trash of its own: the standard
/// per-volume one (`.Trash-<uid>` at its root) would be committed and
/// pushed like any folder of the drive.
pub fn trash(paths: &[PathBuf], drive: Option<&Path>, keep: &[PathBuf], task: &Task) -> Result<Trashed, String> {
    if let Some(p) = paths.iter().find(|p| protected(p, keep)) {
        return Err(t!("ops.cant_delete_here", path = p.display()));
    }
    if cfg!(target_os = "linux") && drive.is_some_and(|d| paths.iter().any(|p| p.starts_with(d))) {
        return Ok(Trashed::NoTrash {
            reason: t!("ops.drive_no_trash"),
            remaining: paths.to_vec(),
        });
    }
    task.set_totals(0, 0, paths.len() as u64);
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::new();
    // Finder's method (the default) goes through AppleScript and asks for
    // permission to control Finder; the file manager's own call needs none.
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    for (i, path) in paths.iter().enumerate() {
        if task.is_cancelled() {
            return Err(CANCELLED.into());
        }
        task.on(path);
        if let Err(e) = ctx.delete_all([path]) {
            return Ok(Trashed::NoTrash { reason: t!("ops.cant_trash", error = e), remaining: paths[i..].to_vec() });
        }
        task.item_done();
    }
    Ok(Trashed::Done)
}

/// Deletes `paths` for good (the user confirmed), entry by entry: a cancel
/// stops between two, and what went before is gone.
pub fn delete_permanently(paths: &[PathBuf], keep: &[PathBuf], task: &Task) -> Result<(), String> {
    if let Some(p) = paths.iter().find(|p| protected(p, keep)) {
        return Err(t!("ops.cant_delete_here", path = p.display()));
    }
    let (_, files) = measure(paths, task).map_err(|_| CANCELLED.to_string())?;
    task.set_totals(0, files, paths.len() as u64);
    for path in paths {
        remove_counted(path, task).map_err(|e| task.explain(e, |e| t!("ops.delete_failed", name = display_name(path), error = crate::i18n::io_text(&e))))?;
        task.item_done();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn names_are_checked_like_the_file_system_does() {
        assert_eq!(check_name("  Reports 2026 "), Ok("Reports 2026"));
        assert!(check_name("   ").is_err());
        assert!(check_name("..").is_err());
        assert!(check_name("a/b").is_err());
        assert!(check_name(&"x".repeat(256)).is_err());
        assert!(check_name(&"x".repeat(255)).is_ok());
    }

    #[test]
    fn a_new_folder_never_replaces_anything() {
        let dir = tempfile::tempdir().unwrap();
        let made = new_folder(dir.path(), " untitled folder ").unwrap();
        assert_eq!(made, dir.path().join("untitled folder"));
        assert!(made.is_dir());
        write(&dir.path().join("taken"), "keep me");
        assert!(new_folder(dir.path(), "untitled folder").unwrap_err().contains("already exists"));
        assert!(new_folder(dir.path(), "taken").is_err());
        assert_eq!(fs::read_to_string(dir.path().join("taken")).unwrap(), "keep me");
    }

    #[test]
    fn free_names_follow_finder() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert_eq!(free_target(d, "a.txt", false, false), d.join("a.txt"), "free: the name itself");
        assert_eq!(free_target(d, "a.txt", false, true), d.join("a copy.txt"), "a duplicate never keeps the name");
        write(&d.join("a.txt"), "");
        write(&d.join("a copy.txt"), "");
        assert_eq!(free_target(d, "a.txt", false, true), d.join("a copy 2.txt"));
        assert_eq!(free_target(d, "a.txt", false, false), d.join("a 2.txt"), "keep both");
        assert_eq!(free_target(d, "v1.2", true, true), d.join("v1.2 copy"), "a folder has no extension");
        assert_eq!(free_target(d, ".env", false, true), d.join(".env copy"), "nor a dotfile");
        std::os::unix::fs::symlink("nowhere", d.join("b")).unwrap();
        assert_eq!(free_target(d, "b", false, false), d.join("b 2"), "a broken link takes its name");
    }

    #[test]
    fn copying_a_folder_keeps_its_tree_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src/project");
        write(&src.join("notes.md"), "hello");
        write(&src.join("deep/er/data.bin"), "bytes");
        std::os::unix::fs::symlink("notes.md", src.join("link")).unwrap();
        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();

        let landed = paste(std::slice::from_ref(&src), &dest, false, &Task::default()).unwrap();
        assert_eq!(landed, vec![dest.join("project")]);
        assert_eq!(fs::read_to_string(dest.join("project/deep/er/data.bin")).unwrap(), "bytes");
        assert_eq!(fs::read_link(dest.join("project/link")).unwrap(), Path::new("notes.md"), "a link stays a link");
        assert!(src.join("notes.md").exists(), "a copy leaves the original");

        let again = paste(std::slice::from_ref(&src), &dest, false, &Task::default()).unwrap();
        assert_eq!(again, vec![dest.join("project 2")], "the name is taken: keep both");
        assert_eq!(names(&dest), ["project", "project 2"]);
    }

    #[test]
    fn copying_into_the_same_folder_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("photo.jpg"), "jpg");
        let src = dir.path().join("photo.jpg");
        assert_eq!(paste(std::slice::from_ref(&src), dir.path(), false, &Task::default()).unwrap(), vec![dir.path().join("photo copy.jpg")]);
        assert_eq!(paste(&[src], dir.path(), false, &Task::default()).unwrap(), vec![dir.path().join("photo copy 2.jpg")]);
    }

    #[test]
    fn moving_renames_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/report.pdf"), "new");
        write(&dir.path().join("b/report.pdf"), "old");
        let landed = paste(&[dir.path().join("a/report.pdf")], &dir.path().join("b"), true, &Task::default()).unwrap();
        assert_eq!(landed, vec![dir.path().join("b/report 2.pdf")]);
        assert_eq!(fs::read_to_string(dir.path().join("b/report.pdf")).unwrap(), "old");
        assert!(!dir.path().join("a/report.pdf").exists());
    }

    #[test]
    fn moving_within_its_own_folder_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.txt"), "x");
        let landed = paste(&[dir.path().join("a.txt")], dir.path(), true, &Task::default()).unwrap();
        assert_eq!(landed, vec![dir.path().join("a.txt")]);
        assert_eq!(names(dir.path()), ["a.txt"]);
    }

    #[test]
    fn a_folder_never_goes_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("f");
        write(&folder.join("inner/x"), "");
        for cut in [false, true] {
            assert!(paste(std::slice::from_ref(&folder), &folder, cut, &Task::default()).unwrap_err().contains("into itself"));
            assert!(paste(std::slice::from_ref(&folder), &folder.join("inner"), cut, &Task::default()).unwrap_err().contains("into itself"));
        }
        // A sibling whose name starts the same is not inside it.
        fs::create_dir(dir.path().join("f2")).unwrap();
        assert!(paste(std::slice::from_ref(&folder), &dir.path().join("f2"), false, &Task::default()).is_ok());
    }

    #[test]
    fn a_failed_copy_leaves_no_half_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        write(&src.join("ok.txt"), "fine");
        // A FIFO can't be copied: the copy fails after "ok.txt" was written.
        let fifo = src.join("zz-pipe");
        assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        assert!(paste(std::slice::from_ref(&src), &dest, false, &Task::default()).is_err());
        assert!(names(&dest).is_empty(), "the partial copy was removed: {:?}", names(&dest));
        assert!(src.join("ok.txt").exists(), "the original is intact");
    }

    #[test]
    fn a_copy_counts_its_bytes_entries_and_items() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        write(&src.join("a/one.bin"), &"x".repeat(5000));
        write(&src.join("two.txt"), "hello");
        write(&dir.path().join("loose.txt"), "abc");
        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let task = Task::default();
        paste(&[src.clone(), dir.path().join("loose.txt")], &dest, false, &task).unwrap();
        let s = task.snapshot();
        assert!(!s.preparing);
        assert_eq!((s.bytes_done, s.bytes_total), (5008, 5008));
        assert_eq!((s.files_done, s.files_total), (5, 5), "src, a, one.bin, two.txt and loose.txt");
        assert_eq!((s.items_done, s.items_total), (2, 2));
    }

    #[test]
    fn a_copied_file_keeps_its_permissions_and_time() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("script.sh");
        write(&src, "#!/bin/sh");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        fs::File::options().write(true).open(&src).unwrap().set_modified(old).unwrap();
        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        paste(std::slice::from_ref(&src), &dest, false, &Task::default()).unwrap();
        let copy = fs::metadata(dest.join("script.sh")).unwrap();
        assert_eq!(copy.permissions().mode() & 0o777, 0o750);
        assert_eq!(copy.modified().unwrap(), old);
        assert_eq!(fs::read_to_string(dest.join("script.sh")).unwrap(), "#!/bin/sh");
    }

    #[test]
    fn a_cancelled_copy_or_delete_leaves_things_as_they_were() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        write(&src.join("deep/file.txt"), "data");
        let dest = dir.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let cancelled = Task::default();
        cancelled.cancel();
        assert_eq!(paste(std::slice::from_ref(&src), &dest, false, &cancelled).unwrap_err(), CANCELLED);
        assert!(names(&dest).is_empty());
        assert!(copy_fresh(&src, &dest.join("src"), &cancelled).is_err());
        assert!(names(&dest).is_empty(), "no half copy");
        assert_eq!(delete_permanently(std::slice::from_ref(&src), &[], &cancelled).unwrap_err(), CANCELLED);
        assert!(src.join("deep/file.txt").exists());
    }

    #[test]
    fn deleting_counts_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("f/a/b.txt"), "b");
        write(&dir.path().join("f/c.txt"), "c");
        let task = Task::default();
        delete_permanently(&[dir.path().join("f")], &[], &task).unwrap();
        let s = task.snapshot();
        assert_eq!((s.files_done, s.files_total, s.items_done), (4, 4, 1));
        assert!(names(dir.path()).is_empty());
    }

    #[test]
    fn a_missing_source_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let err = paste(&[dir.path().join("gone.txt")], dir.path(), false, &Task::default()).unwrap_err();
        assert!(err.contains("no longer there"), "{err}");
    }

    #[test]
    fn deleting_permanently_removes_links_not_their_targets() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("target/keep.txt"), "keep");
        std::os::unix::fs::symlink(dir.path().join("target"), dir.path().join("link")).unwrap();
        write(&dir.path().join("folder/a/b.txt"), "b");
        delete_permanently(&[dir.path().join("link"), dir.path().join("folder")], &[], &Task::default()).unwrap();
        assert_eq!(names(dir.path()), ["target"]);
        assert!(dir.path().join("target/keep.txt").exists());
    }

    #[test]
    fn renaming_keeps_the_content_and_never_takes_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(&d.join("a.txt"), "mine");
        write(&d.join("b.txt"), "theirs");
        let renamed = rename(&d.join("a.txt"), " notes.txt ", &[]).unwrap();
        assert_eq!(renamed, d.join("notes.txt"));
        assert_eq!(fs::read_to_string(&renamed).unwrap(), "mine");

        let err = rename(&renamed, "b.txt", &[]).unwrap_err();
        assert!(err.contains("already taken"), "{err}");
        assert_eq!(fs::read_to_string(d.join("b.txt")).unwrap(), "theirs");
        std::os::unix::fs::symlink("nowhere", d.join("dangling")).unwrap();
        assert!(rename(&renamed, "dangling", &[]).is_err(), "a broken link takes its name");
        assert!(rename(&renamed, "a/b", &[]).is_err());
        assert_eq!(names(d), ["b.txt", "dangling", "notes.txt"]);
    }

    #[test]
    fn renaming_a_folder_takes_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("old/deep/x.md"), "x");
        let renamed = rename(&dir.path().join("old"), "new", &[]).unwrap();
        assert_eq!(fs::read_to_string(renamed.join("deep/x.md")).unwrap(), "x");
        assert_eq!(names(dir.path()), ["new"]);
    }

    #[test]
    fn the_same_name_or_only_its_case_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(&d.join("readme.md"), "r");
        assert_eq!(rename(&d.join("readme.md"), "readme.md", &[]).unwrap(), d.join("readme.md"));
        // On a case-insensitive volume (the macOS default) "README.md" is the file itself.
        assert_eq!(rename(&d.join("readme.md"), "README.md", &[]).unwrap(), d.join("README.md"));
        assert_eq!(names(d), ["README.md"]);
    }

    #[test]
    fn a_hard_link_is_another_name_not_the_same_one() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        write(&d.join("a"), "a");
        fs::hard_link(d.join("a"), d.join("b")).unwrap();
        // rename(2) between two links of one file succeeds and changes nothing.
        assert!(rename(&d.join("a"), "b", &[]).unwrap_err().contains("already taken"));
        assert_eq!(names(d), ["a", "b"]);
    }

    #[test]
    fn roots_and_places_are_never_deleted_nor_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let place = dir.path().join("Documents");
        fs::create_dir(&place).unwrap();
        for p in [PathBuf::from("/"), PathBuf::from("/Users"), place.clone()] {
            assert!(delete_permanently(std::slice::from_ref(&p), std::slice::from_ref(&place), &Task::default()).is_err(), "{}", p.display());
            assert!(trash(std::slice::from_ref(&p), None, std::slice::from_ref(&place), &Task::default()).is_err(), "{}", p.display());
            assert!(rename(&p, "x", std::slice::from_ref(&place)).is_err(), "{}", p.display());
        }
        assert!(place.exists());
    }

    // Linux only: on macOS the call would reach the user's real Trash.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_drive_on_linux_asks_before_deleting() {
        let dir = tempfile::tempdir().unwrap();
        let drive = dir.path().join("GitDrive");
        write(&drive.join("doc.txt"), "x");
        let outcome = trash(&[drive.join("doc.txt")], Some(&drive), &[], &Task::default());
        assert!(matches!(outcome, Ok(Trashed::NoTrash { .. })), "{outcome:?}");
        assert!(drive.join("doc.txt").exists(), "nothing was touched");
    }
}
