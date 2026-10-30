//! Disk figures for the Control Center: the volume that holds the repository
//! and the size of a directory tree (the repository's `.git`); and what the
//! repository's host accepts, against the size of its object store.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// A decimal gigabyte, the unit hosts state their limits in.
pub const GB: u64 = 1_000_000_000;

/// What a repository's host accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCapacity {
    /// `GitHub`, `GitLab.com`, or the remote's host name (empty without a remote).
    pub host: String,
    /// The most the repository should hold: the host's (recommended)
    /// maximum, or `repo_quota_gb`. `None` when neither is known.
    pub limit: Option<u64>,
    /// A lower size the host strongly recommends staying under.
    pub recommended: Option<u64>,
    /// The limit is the user's quota, not the host's.
    pub from_config: bool,
}

/// The host of a remote URL, lowercase: `scheme://[user@]host[:port]/path`
/// or scp's `[user@]host:path`. `None` for a local path or `file://`.
pub fn remote_host(url: &str) -> Option<String> {
    let authority = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme.eq_ignore_ascii_case("file") {
            return None;
        }
        rest.split('/').next()?
    } else {
        // scp form: a colon before any slash.
        let (before, _) = url.split_once(':')?;
        if before.contains('/') {
            return None;
        }
        before
    };
    let host = authority.rsplit('@').next()?;
    let host = host.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The limits of `host`, or the user's quota (`repo_quota_gb`, decimal GB;
/// 0 or absent leaves the host's). GitHub publishes no quota: 10 GB is the
/// maximum its repository-limits page recommends, 5 GB the size it
/// "strongly recommends" staying under (large-files page; only pushes over
/// 2 GB and files over 100 MiB are refused). GitLab.com limits a project to
/// 10 GB.
pub fn repo_capacity(host: Option<&str>, quota_gb: Option<u64>) -> RepoCapacity {
    let (name, limit, recommended) = match host {
        Some("github.com") => ("GitHub".to_string(), Some(10 * GB), Some(5 * GB)),
        Some("gitlab.com") => ("GitLab.com".to_string(), Some(10 * GB), None),
        Some(other) => (other.to_string(), None, None),
        None => (String::new(), None, None),
    };
    match quota_gb.filter(|q| *q > 0) {
        Some(q) => RepoCapacity {
            host: name,
            limit: Some(q * GB),
            recommended: recommended.filter(|r| *r < q * GB),
            from_config: true,
        },
        None => RepoCapacity { host: name, limit, recommended, from_config: false },
    }
}

/// Bytes of a repository's object store (`<git_dir>/objects`, packs and
/// loose objects): what its remote holds, give or take unpushed commits and
/// packing. The rest of `.git` (spool, journal, index) never leaves the machine.
pub fn objects_size(git_dir: &Path) -> u64 {
    dir_size(&git_dir.join("objects"))
}

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
    fn the_host_comes_from_any_remote_url_form() {
        assert_eq!(remote_host("git@github.com:giacomeli/git-drive-files.git").as_deref(), Some("github.com"));
        assert_eq!(remote_host("https://github.com/o/r.git").as_deref(), Some("github.com"));
        assert_eq!(remote_host("https://x-access-token:abc@GitHub.com/o/r").as_deref(), Some("github.com"), "credentials and case");
        assert_eq!(remote_host("ssh://git@gitlab.com:2222/g/p.git").as_deref(), Some("gitlab.com"), "a port");
        assert_eq!(remote_host("git.example.org:repos/drive.git").as_deref(), Some("git.example.org"), "scp form without a user");
        assert_eq!(remote_host("/srv/git/drive.git"), None, "a local path");
        assert_eq!(remote_host("file:///srv/git/drive.git"), None, "a file URL");
        assert_eq!(remote_host("../drive.git"), None);
    }

    #[test]
    fn known_hosts_bring_their_limits_and_a_quota_overrides_them() {
        let github = repo_capacity(Some("github.com"), None);
        assert_eq!(github, RepoCapacity { host: "GitHub".into(), limit: Some(10 * GB), recommended: Some(5 * GB), from_config: false });
        let gitlab = repo_capacity(Some("gitlab.com"), None);
        assert_eq!((gitlab.host.as_str(), gitlab.limit, gitlab.recommended), ("GitLab.com", Some(10 * GB), None));
        let own = repo_capacity(Some("git.example.org"), None);
        assert_eq!((own.host.as_str(), own.limit), ("git.example.org", None), "an unknown host has no limit");
        let quota = repo_capacity(Some("git.example.org"), Some(50));
        assert_eq!((quota.limit, quota.from_config), (Some(50 * GB), true));
        let lower = repo_capacity(Some("github.com"), Some(3));
        assert_eq!((lower.limit, lower.recommended), (Some(3 * GB), None), "a mark above the quota is dropped");
        let higher = repo_capacity(Some("github.com"), Some(20));
        assert_eq!((higher.limit, higher.recommended), (Some(20 * GB), Some(5 * GB)));
        assert_eq!(repo_capacity(None, Some(0)).limit, None, "0 is no quota");
    }

    #[test]
    fn the_repository_size_is_its_object_store() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir_all(git.join("objects/pack")).unwrap();
        std::fs::create_dir_all(git.join("objects/ab")).unwrap();
        std::fs::create_dir_all(git.join("git-drive-spool")).unwrap();
        std::fs::write(git.join("objects/pack/p.pack"), vec![0u8; 700]).unwrap();
        std::fs::write(git.join("objects/ab/cdef"), vec![0u8; 30]).unwrap();
        std::fs::write(git.join("git-drive-spool/big"), vec![0u8; 5000]).unwrap();
        assert_eq!(objects_size(&git), 730, "spool and journal never reach the remote");
    }

    #[test]
    fn volume_reports_total_and_free_space() {
        let dir = tempfile::tempdir().unwrap();
        let v = volume(dir.path()).unwrap();
        assert!(v.total > 0 && v.free > 0 && v.free <= v.total, "{:?}", v);
    }
}
