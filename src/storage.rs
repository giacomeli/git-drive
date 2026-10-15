//! Disk figures for the Control Center: the volume that holds the repository
//! and the size of a directory tree (the repository's `.git`).

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Volume {
    pub total: u64,
    /// Space available to an unprivileged user (`f_bavail`).
    pub free: u64,
}

/// Total and available bytes of the filesystem holding `path`.
pub fn volume(path: &Path) -> std::io::Result<Volume> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let unit = stat.f_frsize as u64;
    Ok(Volume {
        total: stat.f_blocks as u64 * unit,
        free: stat.f_bavail as u64 * unit,
    })
}

/// Bytes of the regular files under `path` (symlinks are not followed;
/// unreadable entries are skipped).
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else { continue };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_size_sums_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("x"), vec![0u8; 10]).unwrap();
        std::fs::write(dir.path().join("a/b/y"), vec![0u8; 1000]).unwrap();
        std::os::unix::fs::symlink("/etc", dir.path().join("a/link")).unwrap();
        assert_eq!(dir_size(dir.path()), 1010, "symlinks are not followed");
    }

    #[test]
    fn volume_reports_total_and_free_space() {
        let dir = tempfile::tempdir().unwrap();
        let v = volume(dir.path()).unwrap();
        assert!(v.total > 0 && v.free > 0 && v.free <= v.total, "{:?}", v);
    }
}
