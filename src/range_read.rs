//! Range reads straight from working-tree files, verified once per session.
//!
//! Large content (chunks, big committed files, big local-only files) is
//! served with `pread` of just the requested range — plus range decryption
//! when sealed — instead of loading the whole file on every FUSE read. Before
//! the first byte is served, the file is checked in streaming against what
//! the caller expects (`Expect`); the open handle is then kept together with
//! the file's identity (dev, inode, size, mtime), and a later identity change
//! forces a new check. Serving from the verified handle means a path replaced
//! after the check (a checkout renaming a new version over it) keeps reading
//! the verified content until the caller's expectation changes.
//!
//! The AEAD tag of a sealed whole file is checked once here; range decryption
//! alone does not authenticate. For chunks, the SHA-256 of the ciphertext
//! against the (authenticated) manifest plays that role.

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::crypt::{Crypto, SEALED_HEADER_LEN, SEALED_TAG_LEN};

/// Read size of the streaming checks: bounds their memory to one block.
const VERIFY_BLOCK: usize = 1 << 20;

/// What a file's content must match before any byte of it is served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Chunk: SHA-256 (lowercase hex) of the bytes on disk, from the manifest.
    Sha256(String),
    /// Committed file: Git blob id at HEAD (SHA-1 object format). A sealed
    /// blob also has its AEAD tag checked: the blob id alone does not prove
    /// it was sealed with our key.
    GitBlob(git2::Oid),
    /// Local-only file: outside Git, nothing to check against. Re-opened when
    /// the path stops pointing at the open handle.
    Unverified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// Content is not the expected version (hash or blob id differ).
    Mismatch,
    /// Structure or tag invalid: truncated, tampered with, wrong key, or a
    /// size that contradicts the caller's expectation.
    Corrupt,
    Io,
}

/// What a range read returns about the file besides the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Content size in the clear (file size minus the seal overhead).
    pub plain_len: u64,
    pub sealed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    len: u64,
    mtime_ns: i128,
}

impl Identity {
    fn of(meta: &std::fs::Metadata) -> Identity {
        Identity {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.size(),
            mtime_ns: meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128,
        }
    }
}

struct OpenFile {
    file: File,
    identity: Identity,
    expect: Expect,
    /// `Some` when sealed: the nonce for range decryption.
    nonce: Option<[u8; 24]>,
    plain_len: u64,
    last_used: u64,
}

/// Verified open handles of working-tree files, keyed by path, in a bounded
/// LRU (never exhausts file descriptors).
pub struct RangeReader {
    open: HashMap<PathBuf, OpenFile>,
    capacity: usize,
    tick: u64,
    verifications: u64,
}

impl RangeReader {
    pub fn new(capacity: usize) -> RangeReader {
        RangeReader {
            open: HashMap::new(),
            capacity: capacity.max(1),
            tick: 0,
            verifications: 0,
        }
    }

    /// Reads `size` bytes at plaintext `offset` of the file at `path`,
    /// checking it against `expect` first if this handle was not checked yet.
    /// `crypto` enables sealed content (a file without the magic is read as
    /// is, like `maybe_open`). `expected_plain_len`, when given, must match
    /// the content size or the file is `Corrupt`.
    pub fn read(
        &mut self,
        path: &Path,
        expect: &Expect,
        crypto: Option<&Crypto>,
        expected_plain_len: Option<u64>,
        offset: u64,
        size: usize,
    ) -> Result<(Vec<u8>, Layout), RangeError> {
        self.tick += 1;
        if !self.still_valid(path, expect) {
            self.open.remove(path);
            let entry = self.open_and_verify(path, expect, crypto)?;
            if self.open.len() >= self.capacity {
                self.evict_lru();
            }
            self.open.insert(path.to_path_buf(), entry);
        }
        let tick = self.tick;
        let entry = self.open.get_mut(path).expect("inserted above");
        entry.last_used = tick;
        let layout = Layout {
            plain_len: entry.plain_len,
            sealed: entry.nonce.is_some(),
        };
        if let Some(expected) = expected_plain_len {
            if expected != entry.plain_len {
                self.open.remove(path);
                return Err(RangeError::Corrupt);
            }
        }
        if offset >= entry.plain_len || size == 0 {
            return Ok((Vec::new(), layout));
        }
        let n = (size as u64).min(entry.plain_len - offset) as usize;
        let mut buf = vec![0u8; n];
        let base = if entry.nonce.is_some() { SEALED_HEADER_LEN as u64 } else { 0 };
        if entry.file.read_exact_at(&mut buf, base + offset).is_err() {
            self.open.remove(path);
            return Err(RangeError::Io);
        }
        if let (Some(nonce), Some(c)) = (entry.nonce, crypto) {
            c.decrypt_range_in_place(&nonce, offset, &mut buf);
        }
        Ok((buf, layout))
    }

    /// Forgets every handle (the caller's expectations changed, e.g. HEAD moved).
    pub fn clear(&mut self) {
        self.open.clear();
    }

    pub fn forget(&mut self, path: &Path) {
        self.open.remove(path);
    }

    /// Closes handles whose path no longer leads to the file they hold
    /// (deleted, collected, replaced): an open handle of an unlinked file
    /// would keep its disk space allocated until evicted.
    pub fn prune_missing(&mut self) {
        self.open.retain(|path, entry| {
            std::fs::metadata(path)
                .map(|m| Identity::of(&m) == entry.identity)
                .unwrap_or(false)
        });
    }

    /// Streaming checks performed so far (introspection for tests).
    pub fn verifications(&self) -> u64 {
        self.verifications
    }

    pub fn open_handles(&self) -> usize {
        self.open.len()
    }

    /// The cached handle can serve `expect` without a new check: same
    /// expectation, file unchanged in place and, for unverified content, the
    /// path still pointing at it.
    fn still_valid(&self, path: &Path, expect: &Expect) -> bool {
        let Some(entry) = self.open.get(path) else {
            return false;
        };
        if entry.expect != *expect {
            return false;
        }
        let Ok(meta) = entry.file.metadata() else {
            return false;
        };
        if Identity::of(&meta) != entry.identity {
            return false;
        }
        if *expect == Expect::Unverified {
            return std::fs::metadata(path)
                .map(|m| Identity::of(&m) == entry.identity)
                .unwrap_or(false);
        }
        true
    }

    fn evict_lru(&mut self) {
        if let Some(oldest) = self
            .open
            .iter()
            .min_by_key(|(_, e)| e.last_used)
            .map(|(p, _)| p.clone())
        {
            self.open.remove(&oldest);
        }
    }

    fn open_and_verify(
        &mut self,
        path: &Path,
        expect: &Expect,
        crypto: Option<&Crypto>,
    ) -> Result<OpenFile, RangeError> {
        let file = File::open(path).map_err(|_| RangeError::Io)?;
        let identity = Identity::of(&file.metadata().map_err(|_| RangeError::Io)?);

        let mut header = [0u8; SEALED_HEADER_LEN];
        let header_len = read_up_to(&file, &mut header, 0)?;
        let nonce = crypto.and_then(|_| Crypto::sealed_nonce(&header[..header_len]));
        let plain_len = match nonce {
            Some(_) => identity
                .len
                .checked_sub((SEALED_HEADER_LEN + SEALED_TAG_LEN) as u64)
                .ok_or(RangeError::Corrupt)?,
            None => identity.len,
        };

        match expect {
            Expect::Unverified => {}
            Expect::Sha256(hex) => {
                self.verifications += 1;
                let mut hasher = Sha256::new();
                stream(&file, identity.len, |_, block| hasher.update(block))?;
                if format!("{:x}", hasher.finalize()) != *hex {
                    return Err(RangeError::Mismatch);
                }
            }
            Expect::GitBlob(oid) => {
                self.verifications += 1;
                // One pass through the handle that will serve the reads: the
                // Git blob id (SHA-1 of "blob <len>\0" + content) and, when
                // sealed, the Poly1305 tag over the ciphertext bytes.
                let mut blob_id = Sha1::new();
                blob_id.update(format!("blob {}\0", identity.len).as_bytes());
                let mut tag_check = match (nonce, crypto) {
                    (Some(n), Some(c)) => Some(c.tag_verifier(&n)),
                    _ => None,
                };
                let ct_start = SEALED_HEADER_LEN as u64;
                let ct_end = ct_start + plain_len;
                stream(&file, identity.len, |pos, block| {
                    blob_id.update(block);
                    if let Some(v) = tag_check.as_mut() {
                        let s = pos.max(ct_start);
                        let e = (pos + block.len() as u64).min(ct_end);
                        if s < e {
                            v.update(&block[(s - pos) as usize..(e - pos) as usize]);
                        }
                    }
                })?;
                if git2::Oid::from_bytes(&blob_id.finalize()).ok() != Some(*oid) {
                    return Err(RangeError::Mismatch);
                }
                if let Some(v) = tag_check {
                    let mut tag = [0u8; SEALED_TAG_LEN];
                    file.read_exact_at(&mut tag, ct_end).map_err(|_| RangeError::Io)?;
                    if !v.verify(&tag) {
                        return Err(RangeError::Corrupt);
                    }
                }
            }
        }
        // Changed in place while being checked: what was hashed may not be
        // what is on disk now.
        if Identity::of(&file.metadata().map_err(|_| RangeError::Io)?) != identity {
            return Err(RangeError::Mismatch);
        }

        Ok(OpenFile {
            file,
            identity,
            expect: expect.clone(),
            nonce,
            plain_len,
            last_used: self.tick,
        })
    }
}

/// Reads up to `buf.len()` bytes at `offset`; returns how many (short only at EOF).
fn read_up_to(file: &File, buf: &mut [u8], offset: u64) -> Result<usize, RangeError> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read_at(&mut buf[filled..], offset + filled as u64) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(RangeError::Io),
        }
    }
    Ok(filled)
}

/// Feeds the first `len` bytes of the file to `f(position, block)`, one
/// `VERIFY_BLOCK` at a time.
fn stream(file: &File, len: u64, mut f: impl FnMut(u64, &[u8])) -> Result<(), RangeError> {
    let mut buf = vec![0u8; VERIFY_BLOCK.min(len as usize).max(1)];
    let mut done: u64 = 0;
    while done < len {
        let n = (len - done).min(buf.len() as u64) as usize;
        file.read_exact_at(&mut buf[..n], done)
            .map_err(|_| RangeError::Io)?;
        f(done, &buf[..n]);
        done += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crypto() -> Crypto {
        Crypto::from_master_key(&[7u8; 32])
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 131 + 17) as u8).collect()
    }

    fn sha(data: &[u8]) -> Expect {
        Expect::Sha256(crate::chunk::sha256_hex(data))
    }

    fn blob(data: &[u8]) -> Expect {
        Expect::GitBlob(git2::Oid::hash_object(git2::ObjectType::Blob, data).unwrap())
    }

    #[test]
    fn plain_file_reads_ranges_after_one_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let data = pattern(5000);
        std::fs::write(&path, &data).unwrap();
        let mut r = RangeReader::new(4);
        let expect = sha(&data);

        let (out, layout) = r.read(&path, &expect, None, Some(5000), 100, 50).unwrap();
        assert_eq!(out, data[100..150]);
        assert_eq!(layout, Layout { plain_len: 5000, sealed: false });
        let (out, _) = r.read(&path, &expect, None, None, 4990, 100).unwrap();
        assert_eq!(out, data[4990..], "short read at EOF");
        let (out, _) = r.read(&path, &expect, None, None, 6000, 10).unwrap();
        assert!(out.is_empty(), "past EOF");
        assert_eq!(r.verifications(), 1, "the check runs once per handle");
    }

    #[test]
    fn sealed_file_decrypts_ranges_and_checks_the_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let c = crypto();
        let data = pattern(3000);
        let sealed = c.seal(&data);
        std::fs::write(&path, &sealed).unwrap();
        let mut r = RangeReader::new(4);

        let (out, layout) = r.read(&path, &blob(&sealed), Some(&c), None, 1234, 700).unwrap();
        assert_eq!(out, data[1234..1934]);
        assert_eq!(layout, Layout { plain_len: 3000, sealed: true });

        // Same blob id but sealed under another key: the tag catches it.
        let other = Crypto::from_master_key(&[9u8; 32]);
        let mut r2 = RangeReader::new(4);
        assert_eq!(
            r2.read(&path, &blob(&sealed), Some(&other), None, 0, 10),
            Err(RangeError::Corrupt)
        );
    }

    #[test]
    fn tampered_sealed_file_is_corrupt_even_with_matching_blob_id() {
        // A tampered blob committed to the repo has a matching blob id (the
        // tree points at it); only the tag can catch it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let c = crypto();
        let mut sealed = c.seal(&pattern(500));
        sealed[SEALED_HEADER_LEN + 10] ^= 0x40;
        std::fs::write(&path, &sealed).unwrap();
        let mut r = RangeReader::new(4);
        assert_eq!(
            r.read(&path, &blob(&sealed), Some(&c), None, 0, 100),
            Err(RangeError::Corrupt)
        );
    }

    #[test]
    fn content_differing_from_expectation_is_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"actual content").unwrap();
        let mut r = RangeReader::new(4);
        assert_eq!(
            r.read(&path, &sha(b"expected content"), None, None, 0, 5),
            Err(RangeError::Mismatch)
        );
        assert_eq!(
            r.read(&path, &blob(b"expected content"), None, None, 0, 5),
            Err(RangeError::Mismatch)
        );
        assert_eq!(r.open_handles(), 0, "nothing unverified stays cached");
    }

    #[test]
    fn file_changed_in_place_after_check_is_checked_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let data = pattern(1000);
        std::fs::write(&path, &data).unwrap();
        let mut r = RangeReader::new(4);
        let expect = sha(&data);
        r.read(&path, &expect, None, None, 0, 10).unwrap();

        // Damage in place (same inode, new size/mtime).
        std::fs::write(&path, b"corrupted").unwrap();
        assert_eq!(r.read(&path, &expect, None, None, 0, 10), Err(RangeError::Mismatch));
        assert_eq!(r.verifications(), 2);
    }

    #[test]
    fn replaced_path_keeps_serving_the_verified_handle() {
        // A checkout renames a new version over the path: the old
        // expectation keeps reading the content it verified.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let v1 = pattern(1000);
        std::fs::write(&path, &v1).unwrap();
        let mut r = RangeReader::new(4);
        let expect_v1 = blob(&v1);
        r.read(&path, &expect_v1, None, None, 0, 10).unwrap();

        let tmp = dir.path().join("f.new");
        let v2 = vec![0xAAu8; 1000];
        std::fs::write(&tmp, &v2).unwrap();
        std::fs::rename(&tmp, &path).unwrap();

        let (out, _) = r.read(&path, &expect_v1, None, None, 500, 10).unwrap();
        assert_eq!(out, v1[500..510]);
        let (out, _) = r.read(&path, &blob(&v2), None, None, 500, 10).unwrap();
        assert_eq!(out, v2[500..510], "the new expectation opens the new file");
    }

    #[test]
    fn unverified_reopens_when_the_path_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"first version").unwrap();
        let mut r = RangeReader::new(4);
        let (out, _) = r.read(&path, &Expect::Unverified, None, None, 0, 5).unwrap();
        assert_eq!(out, b"first");

        let tmp = dir.path().join("f.new");
        std::fs::write(&tmp, b"second version").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        let (out, _) = r.read(&path, &Expect::Unverified, None, None, 0, 6).unwrap();
        assert_eq!(out, b"second");
        assert_eq!(r.verifications(), 0);
    }

    #[test]
    fn size_contradicting_the_caller_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let data = pattern(100);
        std::fs::write(&path, &data).unwrap();
        let mut r = RangeReader::new(4);
        assert_eq!(
            r.read(&path, &sha(&data), None, Some(99), 0, 10),
            Err(RangeError::Corrupt)
        );
    }

    #[test]
    fn sealed_file_shorter_than_its_overhead_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let c = crypto();
        let sealed = c.seal(b"x");
        let short = &sealed[..SEALED_HEADER_LEN + 3];
        std::fs::write(&path, short).unwrap();
        let mut r = RangeReader::new(4);
        assert_eq!(
            r.read(&path, &sha(short), Some(&c), None, 0, 1),
            Err(RangeError::Corrupt)
        );
    }

    #[test]
    fn cleartext_file_with_crypto_on_is_read_as_is() {
        // Mixed state during migration (parity with `maybe_open`).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        let data = pattern(800);
        std::fs::write(&path, &data).unwrap();
        let c = crypto();
        let mut r = RangeReader::new(4);
        let (out, layout) = r.read(&path, &sha(&data), Some(&c), Some(800), 10, 20).unwrap();
        assert_eq!(out, data[10..30]);
        assert!(!layout.sealed);
    }

    #[test]
    fn prune_missing_closes_handles_of_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        let kept = dir.path().join("kept");
        let gone = dir.path().join("gone");
        std::fs::write(&kept, b"kept").unwrap();
        std::fs::write(&gone, b"gone").unwrap();
        let mut r = RangeReader::new(4);
        r.read(&kept, &sha(b"kept"), None, None, 0, 1).unwrap();
        r.read(&gone, &sha(b"gone"), None, None, 0, 1).unwrap();

        std::fs::remove_file(&gone).unwrap();
        r.prune_missing();
        assert_eq!(r.open_handles(), 1);
        assert!(r.open.contains_key(&kept));
    }

    #[test]
    fn lru_bounds_open_handles() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = RangeReader::new(2);
        let mut paths = Vec::new();
        for i in 0..3 {
            let p = dir.path().join(format!("f{}", i));
            std::fs::write(&p, pattern(10 + i)).unwrap();
            paths.push(p);
        }
        r.read(&paths[0], &Expect::Unverified, None, None, 0, 1).unwrap();
        r.read(&paths[1], &Expect::Unverified, None, None, 0, 1).unwrap();
        r.read(&paths[0], &Expect::Unverified, None, None, 0, 1).unwrap();
        r.read(&paths[2], &Expect::Unverified, None, None, 0, 1).unwrap();
        assert_eq!(r.open_handles(), 2);
        assert!(r.open.contains_key(&paths[0]), "most recently used survives");
        assert!(!r.open.contains_key(&paths[1]), "least recently used is closed");
    }
}
