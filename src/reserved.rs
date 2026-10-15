//! Root directories git-drive owns inside the mounted repository. They are
//! committed and synced like anything else, but never shown in the mount and
//! never touched by the drain's sweeps (chunking, encryption migration),
//! which would rename or reseal them out of the form their readers expect.

/// Chunk store of large files (Phase 2.6).
pub const CHUNKS_DIR: &str = crate::chunk::CHUNKS_DIR;
/// Device registry of the P2P notification channel (`.git-drive/peers/<id>`).
pub const GIT_DRIVE_DIR: &str = ".git-drive";

/// Whether `rel_path` (relative to the repository root) is a reserved
/// directory or lies under one. Reserved even in builds without the `p2p`
/// feature: the registry arrives from other machines.
pub fn is_reserved_path(rel_path: &str) -> bool {
    [CHUNKS_DIR, GIT_DRIVE_DIR].iter().any(|dir| {
        rel_path == *dir
            || rel_path
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_dirs_and_their_contents_only() {
        assert!(is_reserved_path(".chunks"));
        assert!(is_reserved_path(".chunks/ab/cdef"));
        assert!(is_reserved_path(".git-drive"));
        assert!(is_reserved_path(".git-drive/peers/abc"));
        assert!(!is_reserved_path(".git-drive-notes.txt"), "prefix of a name is not the dir");
        assert!(!is_reserved_path("docs/.git-drive/x"), "only at the root");
        assert!(!is_reserved_path("doc.txt"));
    }
}
