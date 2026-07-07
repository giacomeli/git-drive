//! Pure core of the encryption at rest (Phase 2.7).
//!
//! The repo (working tree + HEAD) stores content AND names encrypted; the
//! FUSE layer presents everything in the clear. XChaCha20-Poly1305 AEAD with
//! the nonce derived from the plaintext (convergent encryption): same content
//! + same key = same ciphertext — preserves chunk dedup and blob stability in
//! Git. Decisions recorded in the Phase 2.7 planning notes (internal,
//! kept outside the repository).

use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::Path;

/// First line of every encrypted blob (with `\n`): allows mixed state during
/// migration — reads detect clear vs encrypted per file.
pub const CONTENT_MAGIC: &[u8] = b"drive-sync-encrypted v1\n";

const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Filesystem limit for a single name component.
const NAME_COMPONENT_MAX: usize = 255;
/// Conflicted-copy suffix applied by the pull on top of the ENCRYPTED name.
const CONFLICT_MARKER: &str = ".conflict-";

/// Base32 alphabet (RFC 4648) in lowercase — safe on case-insensitive
/// filesystems (APFS default), where base64 would collide by case.
const BASE32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

type HmacSha256 = Hmac<Sha256>;

pub struct Crypto {
    content_cipher: XChaCha20Poly1305,
    name_cipher: XChaCha20Poly1305,
    content_nonce_key: [u8; 32],
    name_nonce_key: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum CryptError {
    /// Blob without the magic, truncated, tampered with, or wrong key.
    OpenFailed,
    /// Encrypted name would exceed the filesystem limit.
    NameTooLong,
}

impl std::fmt::Display for CryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptError::OpenFailed => {
                write!(f, "decryption failed: wrong key or tampered blob")
            }
            CryptError::NameTooLong => {
                write!(f, "encrypted name exceeds the filesystem's 255-byte limit")
            }
        }
    }
}

impl Crypto {
    /// Builds from the 32-byte master key, deriving domain-separated subkeys
    /// via HKDF-SHA256.
    pub fn from_master_key(master: &[u8; 32]) -> Crypto {
        let hk = Hkdf::<Sha256>::new(Some(b"drive-sync v1"), master);
        let mut content_key = [0u8; 32];
        let mut name_key = [0u8; 32];
        let mut content_nonce_key = [0u8; 32];
        let mut name_nonce_key = [0u8; 32];
        hk.expand(b"content-key", &mut content_key).expect("hkdf");
        hk.expand(b"name-key", &mut name_key).expect("hkdf");
        hk.expand(b"content-nonce-key", &mut content_nonce_key)
            .expect("hkdf");
        hk.expand(b"name-nonce-key", &mut name_nonce_key).expect("hkdf");
        Crypto {
            content_cipher: XChaCha20Poly1305::new((&content_key).into()),
            name_cipher: XChaCha20Poly1305::new((&name_key).into()),
            content_nonce_key,
            name_nonce_key,
        }
    }

    /// Deterministic nonce: truncated HMAC(subkey, plaintext). Convergence is
    /// intentional (see decision 3) — the nonce is public (goes in the header).
    fn derive_nonce(key: &[u8; 32], plaintext: &[u8]) -> [u8; NONCE_LEN] {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(key).expect("hmac key");
        mac.update(plaintext);
        let digest = mac.finalize().into_bytes();
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&digest[..NONCE_LEN]);
        nonce
    }

    /// `true` if the bytes carry the encrypted-content header.
    pub fn is_encrypted_bytes(bytes: &[u8]) -> bool {
        bytes.starts_with(CONTENT_MAGIC)
    }

    /// Seals content: `MAGIC || nonce || ciphertext+tag`. Deterministic.
    pub fn seal(&self, plaintext: &[u8]) -> Vec<u8> {
        let nonce = Self::derive_nonce(&self.content_nonce_key, plaintext);
        let ct = self
            .content_cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .expect("aead encrypt does not fail with valid params");
        let mut out = Vec::with_capacity(CONTENT_MAGIC.len() + NONCE_LEN + ct.len());
        out.extend_from_slice(CONTENT_MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// Opens sealed content. Errors on missing magic, truncation, tampering
    /// or wrong key — never returns garbage.
    pub fn open(&self, sealed: &[u8]) -> Result<Vec<u8>, CryptError> {
        let body = sealed
            .strip_prefix(CONTENT_MAGIC)
            .ok_or(CryptError::OpenFailed)?;
        if body.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptError::OpenFailed);
        }
        let (nonce, ct) = body.split_at(NONCE_LEN);
        self.content_cipher
            .decrypt(XNonce::from_slice(nonce), ct)
            .map_err(|_| CryptError::OpenFailed)
    }

    /// Encrypts a name component: deterministic AEAD + lowercase base32.
    pub fn enc_component(&self, clear: &str) -> Result<String, CryptError> {
        let plaintext = clear.as_bytes();
        let nonce = Self::derive_nonce(&self.name_nonce_key, plaintext);
        let ct = self
            .name_cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .expect("aead encrypt does not fail with valid params");
        let mut raw = Vec::with_capacity(NONCE_LEN + ct.len());
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ct);
        let encoded = base32_encode(&raw);
        if encoded.len() > NAME_COMPONENT_MAX {
            return Err(CryptError::NameTooLong);
        }
        Ok(encoded)
    }

    /// Decrypts a component. `None` = not a valid encrypted component
    /// (caller falls back to the literal — mixed state, `.gitkeep`, `.chunks`).
    pub fn dec_component(&self, encoded: &str) -> Option<String> {
        let raw = base32_decode(encoded)?;
        if raw.len() < NONCE_LEN + TAG_LEN {
            return None;
        }
        let (nonce, ct) = raw.split_at(NONCE_LEN);
        let plain = self
            .name_cipher
            .decrypt(XNonce::from_slice(nonce), ct)
            .ok()?;
        String::from_utf8(plain).ok()
    }

    /// Encrypts a path (or symlink target) component by component.
    /// Navigation components (``, `.`, `..`) stay literal — a relative target
    /// `../x` keeps working inside the encrypted tree.
    pub fn enc_path(&self, clear: &str) -> Result<String, CryptError> {
        let parts: Result<Vec<String>, CryptError> = clear
            .split('/')
            .map(|c| match c {
                "" | "." | ".." => Ok(c.to_string()),
                other => {
                    // Exact inverse of dec_path: the conflicted-copy suffix
                    // stays clear on top of the encrypted component.
                    if let Some(idx) = other.find(CONFLICT_MARKER) {
                        let (prefix, suffix) = other.split_at(idx);
                        Ok(format!("{}{}", self.enc_component(prefix)?, suffix))
                    } else {
                        self.enc_component(other)
                    }
                }
            })
            .collect();
        Ok(parts?.join("/"))
    }

    /// Decrypts a path component by component, with a literal fallback for
    /// non-encrypted components and handling of the conflicted-copy suffix
    /// (`<encrypted>.conflict-...` becomes `<clear>.conflict-...`).
    pub fn dec_path(&self, cipher: &str) -> String {
        cipher
            .split('/')
            .map(|c| self.dec_component_lossy(c))
            .collect::<Vec<_>>()
            .join("/")
    }

    fn dec_component_lossy(&self, component: &str) -> String {
        if let Some(clear) = self.dec_component(component) {
            return clear;
        }
        // Conflicted copy: clear suffix applied on top of an encrypted name.
        if let Some(idx) = component.find(CONFLICT_MARKER) {
            let (prefix, suffix) = component.split_at(idx);
            if let Some(clear) = self.dec_component(prefix) {
                return format!("{}{}", clear, suffix);
            }
        }
        // Literal fallback: mixed state during migration and internal artifacts.
        component.to_string()
    }
}

/// Generates a fresh key file: 64 hex chars, permissions 0600. Refuses to
/// overwrite unless `force`.
pub fn generate_key_file(path: &Path, force: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if path.exists() && !force {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "key file already exists (use --force to overwrite — the old key will be LOST)",
        ));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut master = [0u8; 32];
    getrandom::getrandom(&mut master)
        .map_err(|e| std::io::Error::other(format!("getrandom: {}", e)))?;
    let hex: String = master.iter().map(|b| format!("{:02x}", b)).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let mut file = opts.open(path)?;
    file.write_all(hex.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Loads the master key from the file. Precise errors: missing file,
/// permissions too open (group/other), invalid format.
pub fn load_key_file(path: &Path) -> std::io::Result<[u8; 32]> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("key file inaccessible at {}: {}", path.display(), e),
        )
    })?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err(std::io::Error::other(format!(
            "key file {} has insecure permissions (expected 0600) — run: chmod 600 on it",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)?;
    let hex = text.trim();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(std::io::Error::other(
            "invalid key file: expected 64 hex characters",
        ));
    }
    let mut master = [0u8; 32];
    for i in 0..32 {
        master[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| std::io::Error::other("invalid hex in key file"))?;
    }
    Ok(master)
}

/// Lowercase RFC 4648 base32, no padding.
fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let bits = u64::from(buf[0]) << 32
            | u64::from(buf[1]) << 24
            | u64::from(buf[2]) << 16
            | u64::from(buf[3]) << 8
            | u64::from(buf[4]);
        let n_chars = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..n_chars {
            let idx = ((bits >> (35 - i * 5)) & 0x1f) as usize;
            out.push(BASE32_ALPHABET[idx] as char);
        }
    }
    out
}

/// Decodes lowercase base32 without padding. `None` for any character
/// outside the alphabet or an impossible length.
fn base32_decode(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() {
        return None;
    }
    // Valid lengths per 5-byte block: 2, 4, 5, 7, 8 chars.
    if matches!(text.len() % 8, 1 | 3 | 6) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    for chunk in text.as_bytes().chunks(8) {
        let mut bits: u64 = 0;
        for (i, &c) in chunk.iter().enumerate() {
            let val = BASE32_ALPHABET.iter().position(|&a| a == c)? as u64;
            bits |= val << (35 - i * 5);
        }
        let n_bytes = match chunk.len() {
            2 => 1,
            4 => 2,
            5 => 3,
            7 => 4,
            8 => 5,
            _ => return None,
        };
        for i in 0..n_bytes {
            out.push(((bits >> (32 - i * 8)) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> Crypto {
        Crypto::from_master_key(&[7u8; 32])
    }

    fn other_crypto() -> Crypto {
        Crypto::from_master_key(&[9u8; 32])
    }

    #[test]
    fn seal_open_roundtrip() {
        let c = crypto();
        let plain = b"sensitive drive content";
        let sealed = c.seal(plain);
        assert!(Crypto::is_encrypted_bytes(&sealed));
        assert!(!Crypto::is_encrypted_bytes(plain));
        assert_eq!(c.open(&sealed).unwrap(), plain.to_vec());
    }

    #[test]
    fn seal_is_deterministic_and_content_sensitive() {
        let c = crypto();
        assert_eq!(c.seal(b"same"), c.seal(b"same"), "convergent: same plaintext = same blob");
        assert_ne!(c.seal(b"one"), c.seal(b"other"));
        // Different keys diverge even for identical plaintext
        assert_ne!(crypto().seal(b"x"), other_crypto().seal(b"x"));
    }

    #[test]
    fn open_rejects_wrong_key_truncation_and_tamper() {
        let c = crypto();
        let sealed = c.seal(b"data");
        assert_eq!(other_crypto().open(&sealed), Err(CryptError::OpenFailed));
        assert_eq!(c.open(&sealed[..sealed.len() - 1]), Err(CryptError::OpenFailed));
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(c.open(&tampered), Err(CryptError::OpenFailed));
        assert_eq!(c.open(b"no magic"), Err(CryptError::OpenFailed));
    }

    #[test]
    fn name_component_roundtrip_and_determinism() {
        let c = crypto();
        let enc = c.enc_component("Résumé Café.pdf").unwrap();
        assert_eq!(enc, c.enc_component("Résumé Café.pdf").unwrap());
        assert_eq!(c.dec_component(&enc).unwrap(), "Résumé Café.pdf");
        // Only lowercase letters and digits from the base32 alphabet (APFS-safe)
        assert!(enc.bytes().all(|b| BASE32_ALPHABET.contains(&b)));
        // Wrong key does not decrypt
        assert_eq!(other_crypto().dec_component(&enc), None);
    }

    #[test]
    fn path_roundtrip_preserves_hierarchy() {
        let c = crypto();
        let enc = c.enc_path("docs/2026/Résumé.pdf").unwrap();
        assert_eq!(enc.matches('/').count(), 2, "1:1 hierarchy");
        assert_eq!(c.dec_path(&enc), "docs/2026/Résumé.pdf");
    }

    #[test]
    fn dec_path_falls_back_to_literal_for_clear_components() {
        let c = crypto();
        // Mixed state: encrypted dir with a literal artifact inside
        let dir = c.enc_component("projects").unwrap();
        let mixed = format!("{}/.gitkeep", dir);
        assert_eq!(c.dec_path(&mixed), "projects/.gitkeep");
        assert_eq!(c.dec_path("clear-name.txt"), "clear-name.txt");
    }

    #[test]
    fn dec_path_handles_conflict_suffix_on_cipher_name() {
        let c = crypto();
        let enc = c.enc_component("note.txt").unwrap();
        let conflicted = format!("{}.conflict-mac-123", enc);
        assert_eq!(c.dec_path(&conflicted), "note.txt.conflict-mac-123");
    }

    #[test]
    fn enc_component_rejects_names_that_would_exceed_fs_limit() {
        let c = crypto();
        let long = "a".repeat(200);
        assert_eq!(c.enc_component(&long), Err(CryptError::NameTooLong));
        // And the practical limit (~119 bytes) passes
        let ok = "b".repeat(110);
        assert!(c.enc_component(&ok).is_ok());
    }

    #[test]
    fn enc_path_keeps_navigation_components_literal() {
        let c = crypto();
        let enc = c.enc_path("../docs/note.txt").unwrap();
        assert!(enc.starts_with("../"));
        assert_eq!(c.dec_path(&enc), "../docs/note.txt");
        let abs = c.enc_path("/tmp/x").unwrap();
        assert!(abs.starts_with('/'));
        assert_eq!(c.dec_path(&abs), "/tmp/x");
    }

    #[test]
    fn base32_roundtrip_various_lengths() {
        for len in [1usize, 2, 3, 4, 5, 6, 39, 40, 41, 64] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 256) as u8).collect();
            let enc = base32_encode(&data);
            assert_eq!(base32_decode(&enc).unwrap(), data, "len {}", len);
        }
        assert_eq!(base32_decode("ABC"), None, "uppercase outside the alphabet");
        assert_eq!(base32_decode("a"), None, "impossible length");
        assert_eq!(base32_decode(""), None);
    }

    #[test]
    fn key_file_generate_load_roundtrip_and_perms() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drive.key");
        generate_key_file(&path, false).unwrap();

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let master = load_key_file(&path).unwrap();
        let c = Crypto::from_master_key(&master);
        assert_eq!(c.open(&c.seal(b"x")).unwrap(), b"x".to_vec());

        // Does not overwrite without force; with force generates a DIFFERENT key
        assert!(generate_key_file(&path, false).is_err());
        generate_key_file(&path, true).unwrap();
        assert_ne!(load_key_file(&path).unwrap(), master);
    }

    #[test]
    fn key_file_rejects_insecure_permissions_and_bad_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drive.key");
        generate_key_file(&path, false).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_key_file(&path).is_err(), "open permissions must be rejected");

        use std::os::unix::fs::PermissionsExt;
        let bad = dir.path().join("bad.key");
        std::fs::write(&bad, "definitely not hex").unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_key_file(&bad).is_err());
    }
}
