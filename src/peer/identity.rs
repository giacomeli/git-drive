//! The device's identity on the channel: a 32-byte secret (the iroh secret
//! key seed), stored like the drive key — 64 hex characters, mode 0600. Its
//! public key is the device id in the registry.

use std::path::Path;

/// Loads the device secret, creating it on first use. A file that exists but
/// does not parse is an error, never silently replaced: a new secret is a new
/// device id, and the old registry entry would point at nobody.
pub fn load_or_create(path: &Path) -> std::io::Result<[u8; 32]> {
    if !path.exists() {
        crate::crypt::generate_key_file(path, false)?;
    }
    crate::crypt::load_key_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn creates_a_0600_key_once_and_reads_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/peer.key");
        let first = load_or_create(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(load_or_create(&path).unwrap(), first, "stable across boots");
    }

    #[test]
    fn a_corrupt_key_is_an_error_not_a_new_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peer.key");
        std::fs::write(&path, "garbage").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_or_create(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "garbage", "never overwritten");
    }
}
