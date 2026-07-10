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
/// Maximum length of an encrypted component ON DISK before it collapses into
/// a hashed `dslong_` entry. The binding limit is NOT the per-component
/// NAME_MAX (255): it is macOS's PATH_MAX of 1024 bytes for the FULL path.
/// Every encrypted component costs at least ~66 chars
/// (base32 of nonce 24 + tag 16 + name), so a deep tree blows past 1024 even
/// with all components under 255 — the 2026-07-10 incident: the Mac replica
/// could not check out the Arch backup ("File name too long" on an ~900-char
/// path). At 80, components of cleartext names up to 10 bytes stay direct and
/// anything longer becomes a fixed 39-char `dslong_` (SHORTER than its direct
/// form), giving a worst case of ~81 bytes per level — realistic trees fit
/// comfortably; hashed-heavy deep trees cost ~40 per level.
pub const LONG_NAME_THRESHOLD: usize = 80;
/// Conflicted-copy suffix applied by the pull on top of the ENCRYPTED name.
const CONFLICT_MARKER: &str = ".conflict-";
/// Prefix of a hashed long-name entry: used when the base32 encrypted name
/// would exceed `LONG_NAME_THRESHOLD`. The full encrypted name is stored in a
/// sidecar `<disk>.name`. The prefix contains `_`, which is outside the base32
/// alphabet, so it can never collide with a normal encrypted component and is
/// safe on case-insensitive filesystems (APFS).
pub const LONG_NAME_PREFIX: &str = "dslong_";
/// Suffix of the sidecar file that carries the full encrypted name.
pub const SIDECAR_SUFFIX: &str = ".name";
/// Bytes of HMAC kept for the hashed long name (160-bit -> 32 base32 chars).
const LONG_HASH_BYTES: usize = 20;

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
}

impl std::fmt::Display for CryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptError::OpenFailed => {
                write!(f, "decryption failed: wrong key or tampered blob")
            }
        }
    }
}

/// One encrypted path component as it should exist on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncName {
    /// Name to use on disk. Always `<= LONG_NAME_THRESHOLD` bytes.
    pub disk: String,
    /// When the encoded name overflowed the component limit: the full base32
    /// encrypted name, to be written into the sidecar `<disk>.name`. `None`
    /// when the name fits inline (the common case).
    pub sidecar: Option<String>,
}

/// A long-name sidecar to materialize on disk: a small file mapping the hashed
/// on-disk name back to the full encrypted name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    /// Full on-disk (encrypted) path of the `<disk>.name` sidecar file.
    pub path: String,
    /// File content: the full base32 encrypted component name.
    pub content: String,
}

/// An encrypted path plus every sidecar that must exist for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncPath {
    /// The on-disk (encrypted) path.
    pub disk: String,
    /// Sidecars for any long components along the path.
    pub sidecars: Vec<Sidecar>,
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
    ///
    /// Never fails on length: when the encoded name would exceed
    /// `LONG_NAME_THRESHOLD`, the on-disk name becomes a bounded hash
    /// (`dslong_<hash>`) and the full encrypted name is returned as a sidecar,
    /// so no file is ever dropped or written in cleartext — and the full path
    /// stays within macOS's 1024-byte PATH_MAX for realistic tree depths.
    pub fn enc_component(&self, clear: &str) -> EncName {
        let encoded = self.enc_component_unbounded(clear);
        if encoded.len() > LONG_NAME_THRESHOLD {
            EncName {
                disk: self.long_disk_name(&encoded),
                sidecar: Some(encoded),
            }
        } else {
            EncName {
                disk: encoded,
                sidecar: None,
            }
        }
    }

    /// Raw encrypted+encoded form of a component, WITHOUT the length bound.
    /// Internal to `enc_component`; also used by the migration tests to
    /// fabricate pre-threshold ("old format") worktree entries.
    pub(crate) fn enc_component_unbounded(&self, clear: &str) -> String {
        let plaintext = clear.as_bytes();
        let nonce = Self::derive_nonce(&self.name_nonce_key, plaintext);
        let ct = self
            .name_cipher
            .encrypt(XNonce::from_slice(&nonce), plaintext)
            .expect("aead encrypt does not fail with valid params");
        let mut raw = Vec::with_capacity(NONCE_LEN + ct.len());
        raw.extend_from_slice(&nonce);
        raw.extend_from_slice(&ct);
        base32_encode(&raw)
    }

    /// Deterministic bounded on-disk name for an over-long encoded component:
    /// `dslong_` + base32(HMAC(name-nonce-key, encoded)). Convergent (same
    /// input -> same name), preserving dedup and staying merge-friendly.
    fn long_disk_name(&self, encoded: &str) -> String {
        let mut mac =
            <HmacSha256 as Mac>::new_from_slice(&self.name_nonce_key).expect("hmac key");
        mac.update(encoded.as_bytes());
        let digest = mac.finalize().into_bytes();
        format!(
            "{}{}",
            LONG_NAME_PREFIX,
            base32_encode(&digest[..LONG_HASH_BYTES])
        )
    }

    /// Encrypts one component, preserving a trailing conflicted-copy suffix
    /// (`.conflict-...`) applied on top of the encrypted name.
    pub(crate) fn enc_component_keep_conflict(&self, other: &str) -> EncName {
        if let Some(idx) = other.find(CONFLICT_MARKER) {
            let (prefix, suffix) = other.split_at(idx);
            let e = self.enc_component(prefix);
            EncName {
                disk: format!("{}{}", e.disk, suffix),
                sidecar: e.sidecar,
            }
        } else {
            self.enc_component(other)
        }
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

    /// Encrypts a path (or symlink target) component by component, returning
    /// only the on-disk path. Navigation components (``, `.`, `..`) stay
    /// literal — a relative target `../x` keeps working inside the encrypted
    /// tree. Deterministic, so it is safe for lookups without touching
    /// sidecars (the sidecars are created by the write path via
    /// `enc_path_full`).
    pub fn enc_path(&self, clear: &str) -> String {
        clear
            .split('/')
            .map(|c| match c {
                "" | "." | ".." => c.to_string(),
                other => self.enc_component_keep_conflict(other).disk,
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Like `enc_path`, but also returns every sidecar that must exist on disk
    /// for the long components along the path. Used by the write path so no
    /// name is dropped or leaked in cleartext.
    pub fn enc_path_full(&self, clear: &str) -> EncPath {
        let mut disk_parts: Vec<String> = Vec::new();
        let mut sidecars: Vec<Sidecar> = Vec::new();
        for c in clear.split('/') {
            match c {
                "" | "." | ".." => disk_parts.push(c.to_string()),
                other => {
                    let e = self.enc_component_keep_conflict(other);
                    disk_parts.push(e.disk.clone());
                    if let Some(content) = e.sidecar {
                        // Sidecar lives beside its component: same directory,
                        // name = `<disk>.name`.
                        let mut sc = disk_parts.clone();
                        let last = sc.len() - 1;
                        sc[last] = format!("{}{}", e.disk, SIDECAR_SUFFIX);
                        sidecars.push(Sidecar {
                            path: sc.join("/"),
                            content,
                        });
                    }
                }
            }
        }
        EncPath {
            disk: disk_parts.join("/"),
            sidecars,
        }
    }

    /// Reverses a hashed long-name on-disk component given the content of its
    /// sidecar (the full base32 encrypted name). A trailing conflicted-copy
    /// suffix on the on-disk name is reapplied to the cleartext.
    pub fn dec_long(&self, disk_component: &str, sidecar_content: &str) -> Option<String> {
        let clear = self.dec_component(sidecar_content)?;
        match disk_component.find(CONFLICT_MARKER) {
            Some(idx) => Some(format!("{}{}", clear, &disk_component[idx..])),
            None => Some(clear),
        }
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

/// True if `name` is a hashed long-name entry (a real file/dir on disk whose
/// cleartext name is recovered from its sidecar).
pub fn is_long_name(name: &str) -> bool {
    name.starts_with(LONG_NAME_PREFIX) && !name.ends_with(SIDECAR_SUFFIX)
}

/// True if `name` is a long-name sidecar (metadata; hidden from the drive view).
pub fn is_long_sidecar(name: &str) -> bool {
    name.starts_with(LONG_NAME_PREFIX) && name.ends_with(SIDECAR_SUFFIX)
}

/// Sidecar file name for a given long on-disk component name.
pub fn sidecar_name(disk_component: &str) -> String {
    format!("{}{}", disk_component, SIDECAR_SUFFIX)
}

/// If `disk_component` is a conflicted-copy of a long name
/// (`dslong_<hash>.conflict-...`), the base `dslong_<hash>` without the suffix,
/// whose sidecar (`dslong_<hash>.name`) holds the encrypted name. `None` when
/// there is no conflict suffix.
pub fn long_name_base(disk_component: &str) -> Option<&str> {
    if !disk_component.starts_with(LONG_NAME_PREFIX) {
        return None;
    }
    disk_component
        .find(CONFLICT_MARKER)
        .map(|idx| &disk_component[..idx])
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
        // "Café.pdf" is 9 UTF-8 bytes — within the 10-byte direct budget of
        // LONG_NAME_THRESHOLD (longer names go through the dslong sidecar,
        // covered by the medium/long tests).
        let enc = c.enc_component("Café.pdf").disk;
        assert_eq!(enc, c.enc_component("Café.pdf").disk);
        assert!(c.enc_component("Café.pdf").sidecar.is_none());
        assert_eq!(c.dec_component(&enc).unwrap(), "Café.pdf");
        // Only lowercase letters and digits from the base32 alphabet (APFS-safe)
        assert!(enc.bytes().all(|b| BASE32_ALPHABET.contains(&b)));
        // Wrong key does not decrypt
        assert_eq!(other_crypto().dec_component(&enc), None);
    }

    #[test]
    fn path_roundtrip_preserves_hierarchy() {
        let c = crypto();
        let enc = c.enc_path("docs/2026/Nota.pdf");
        assert_eq!(enc.matches('/').count(), 2, "1:1 hierarchy");
        assert_eq!(c.dec_path(&enc), "docs/2026/Nota.pdf");
    }

    #[test]
    fn dec_path_falls_back_to_literal_for_clear_components() {
        let c = crypto();
        // Mixed state: encrypted dir with a literal artifact inside
        let dir = c.enc_component("projects").disk;
        let mixed = format!("{}/.gitkeep", dir);
        assert_eq!(c.dec_path(&mixed), "projects/.gitkeep");
        assert_eq!(c.dec_path("clear-name.txt"), "clear-name.txt");
    }

    #[test]
    fn dec_path_handles_conflict_suffix_on_cipher_name() {
        let c = crypto();
        let enc = c.enc_component("note.txt").disk;
        let conflicted = format!("{}.conflict-mac-123", enc);
        assert_eq!(c.dec_path(&conflicted), "note.txt.conflict-mac-123");
    }

    #[test]
    fn long_name_uses_bounded_sidecar_and_roundtrips() {
        let c = crypto();
        // A name too long to encode inline (~119 bytes is the inline limit).
        let long = "a".repeat(200);
        let e = c.enc_component(&long);
        assert!(e.disk.starts_with(LONG_NAME_PREFIX), "disk: {}", e.disk);
        assert!(e.disk.len() <= LONG_NAME_THRESHOLD);
        let sidecar = e.sidecar.expect("long name must carry a sidecar");
        // The sidecar content is the full encrypted name; it round-trips.
        assert_eq!(c.dec_component(&sidecar).unwrap(), long);
        assert_eq!(c.dec_long(&e.disk, &sidecar).unwrap(), long);
        // Deterministic: same clear -> same disk name and same sidecar.
        let e2 = c.enc_component(&long);
        assert_eq!(e2.disk, e.disk);
        assert_eq!(e2.sidecar.as_deref(), Some(sidecar.as_str()));

        // A name within the inline threshold (cleartext <= 10 bytes at
        // LONG_NAME_THRESHOLD = 80) does NOT use a sidecar.
        let ok = c.enc_component(&"b".repeat(10));
        assert!(ok.sidecar.is_none());
        assert!(!ok.disk.starts_with(LONG_NAME_PREFIX));
    }

    #[test]
    fn medium_name_over_threshold_gets_sidecar_and_stays_bounded() {
        let c = crypto();
        // 30 cleartext bytes encode to ~112 chars — under NAME_MAX (255) but
        // over LONG_NAME_THRESHOLD: must collapse to a bounded dslong entry
        // (macOS PATH_MAX regression guard, 2026-07-10).
        let medium = "m".repeat(30);
        let e = c.enc_component(&medium);
        assert!(e.disk.starts_with(LONG_NAME_PREFIX), "disk: {}", e.disk);
        assert!(e.disk.len() <= LONG_NAME_THRESHOLD);
        let sidecar = e.sidecar.expect("over-threshold name carries a sidecar");
        assert_eq!(c.dec_long(&e.disk, &sidecar).unwrap(), medium);
    }

    #[test]
    fn every_component_of_deep_path_stays_within_threshold() {
        let c = crypto();
        // 12 levels of realistic wordpress-ish names: no disk component may
        // exceed the threshold, or deep trees blow past macOS PATH_MAX (1024).
        let clear = (0..12)
            .map(|i| format!("wp-content-uploads-backup-{i}"))
            .collect::<Vec<_>>()
            .join("/");
        let disk = c.enc_path(&clear);
        for comp in disk.split('/') {
            assert!(
                comp.len() <= LONG_NAME_THRESHOLD,
                "component too long ({}): {}",
                comp.len(),
                comp
            );
        }
        // Full relative path fits macOS PATH_MAX with a generous prefix.
        assert!(disk.len() + 100 < 1024, "path too long: {}", disk.len());
    }

    #[test]
    fn enc_path_full_emits_sidecar_beside_long_component() {
        let c = crypto();
        let long = "x".repeat(200);
        let clear = format!("docs/{}/file.txt", long);
        let full = c.enc_path_full(&clear);
        // One sidecar, for the long middle component.
        assert_eq!(full.sidecars.len(), 1);
        let sc = &full.sidecars[0];
        // Sidecar sits in the same directory as its component and is named
        // `<disk>.name`.
        assert!(sc.path.ends_with(SIDECAR_SUFFIX));
        let disk_dir = full.disk.rsplit_once('/').unwrap().0; // docs/<long>
        let sc_dir = sc.path.rsplit_once('/').unwrap().0;
        assert_eq!(sc_dir, disk_dir.rsplit_once('/').unwrap().0); // docs/<enc>
        // The middle component on disk is the hashed long name.
        assert!(disk_dir.contains(LONG_NAME_PREFIX));
        // is_long_sidecar / is_long_name classify correctly.
        let sc_leaf = sc.path.rsplit('/').next().unwrap();
        assert!(is_long_sidecar(sc_leaf));
        assert!(!is_long_name(sc_leaf));
    }

    #[test]
    fn dec_long_reapplies_conflict_suffix() {
        let c = crypto();
        let long = "z".repeat(200);
        let e = c.enc_component(&long);
        let sidecar = e.sidecar.unwrap();
        // A merge appends `.conflict-...` on top of the hashed on-disk name;
        // the original sidecar still resolves it, suffix reapplied.
        let conflicted = format!("{}.conflict-mac-1", e.disk);
        assert_eq!(
            c.dec_long(&conflicted, &sidecar).unwrap(),
            format!("{}.conflict-mac-1", long)
        );
        assert_eq!(long_name_base(&conflicted), Some(e.disk.as_str()));
        assert_eq!(long_name_base(&e.disk), None);
    }

    #[test]
    fn enc_path_keeps_navigation_components_literal() {
        let c = crypto();
        let enc = c.enc_path("../docs/note.txt");
        assert!(enc.starts_with("../"));
        assert_eq!(c.dec_path(&enc), "../docs/note.txt");
        let abs = c.enc_path("/tmp/x");
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
