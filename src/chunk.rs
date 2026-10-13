//! Pure core of the large-file chunking (Phase 2.6).
//!
//! A file above the threshold becomes a text manifest at the original path
//! and the content is sliced into `.chunks/<2 hex>/<sha256>` — each chunk
//! below the remote's per-file limit (GitHub: 100 MB). See decisions 1-5 in
//! the Phase 2.6 planning notes (internal, kept outside the repository).

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// First line of every manifest. Carries the project and format version —
/// a new version requires a new parser (parse rejects unknown ones).
/// Keeps the pre-rename project name (drive-sync): this string lives in
/// committed blobs of existing repos, so renaming it would break them.
pub const MANIFEST_MAGIC: &str = "drive-sync-chunked v1";

/// Reserved chunks directory, at the repo root.
pub const CHUNKS_DIR: &str = ".chunks";

/// Plausible ceiling for a manifest's size — lazy detection only reads blobs
/// up to this limit (covers files of ~2.8 TB with 80 MiB chunks).
pub const MANIFEST_MAX_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub total_size: u64,
    pub chunk_size: u64,
    /// SHA-256 (lowercase hex) of each chunk, in file order.
    pub hashes: Vec<String>,
}

impl Manifest {
    /// `true` if the bytes start with the magic line (cheap detection, no parse).
    pub fn is_manifest_bytes(bytes: &[u8]) -> bool {
        let magic = MANIFEST_MAGIC.as_bytes();
        bytes.len() > magic.len()
            && bytes.starts_with(magic)
            && bytes[magic.len()] == b'\n'
    }

    pub fn parse(bytes: &[u8]) -> Result<Manifest, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "manifest is not UTF-8".to_string())?;
        let mut lines = text.lines();
        match lines.next() {
            Some(l) if l == MANIFEST_MAGIC => {}
            Some(l) => return Err(format!("unknown magic/version: {:?}", l)),
            None => return Err("empty manifest".to_string()),
        }
        let total_size = parse_field(lines.next(), "size")?;
        let chunk_size = parse_field(lines.next(), "chunk-size")?;
        if chunk_size == 0 {
            return Err("chunk-size is zero".to_string());
        }
        let mut hashes = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let hash = line
                .strip_prefix("sha256 ")
                .ok_or_else(|| format!("unexpected line in manifest: {:?}", line))?;
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("malformed hash: {:?}", hash));
            }
            hashes.push(hash.to_ascii_lowercase());
        }
        let expected = total_size.div_ceil(chunk_size) as usize;
        if hashes.len() != expected {
            return Err(format!(
                "inconsistent manifest: {} hash(es) for size {} / chunk-size {} (expected {})",
                hashes.len(),
                total_size,
                chunk_size,
                expected
            ));
        }
        Ok(Manifest {
            total_size,
            chunk_size,
            hashes,
        })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut out = String::new();
        out.push_str(MANIFEST_MAGIC);
        out.push('\n');
        out.push_str(&format!("size {}\n", self.total_size));
        out.push_str(&format!("chunk-size {}\n", self.chunk_size));
        for h in &self.hashes {
            out.push_str(&format!("sha256 {}\n", h));
        }
        out.into_bytes()
    }

    /// Chunk indices covering `[offset, offset+size)`. Empty range if the
    /// offset is past the end.
    pub fn chunks_for_range(&self, offset: u64, size: u64) -> std::ops::Range<usize> {
        if offset >= self.total_size || size == 0 {
            return 0..0;
        }
        let end_byte = offset.saturating_add(size).min(self.total_size);
        let first = (offset / self.chunk_size) as usize;
        let last = end_byte.div_ceil(self.chunk_size) as usize;
        first..last.min(self.hashes.len())
    }
}

fn parse_field(line: Option<&str>, name: &str) -> Result<u64, String> {
    let line = line.ok_or_else(|| format!("missing field {}", name))?;
    let value = line
        .strip_prefix(&format!("{} ", name))
        .ok_or_else(|| format!("expected field {}, got {:?}", name, line))?;
    value
        .parse::<u64>()
        .map_err(|_| format!("invalid value in {}: {:?}", name, value))
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// Relative path of a chunk inside the repo: `.chunks/<2 hex>/<hash>`.
pub fn shard_rel_path(hash: &str) -> PathBuf {
    PathBuf::from(CHUNKS_DIR).join(&hash[..2]).join(hash)
}

/// `true` if the relative path lives inside `.chunks/`.
pub fn is_chunk_path(rel_path: &str) -> bool {
    rel_path == CHUNKS_DIR || rel_path.starts_with(".chunks/")
}

/// Slices `reader` into chunks of `chunk_size`, writing each one to
/// `repo_root/.chunks/<shard>/<hash>` (an existing chunk is not rewritten —
/// dedup by content) and returns the manifest. RAM bounded to one chunk.
pub fn chunk_reader_into_repo(
    reader: &mut impl Read,
    chunk_size: u64,
    repo_root: &Path,
) -> std::io::Result<Manifest> {
    chunk_reader_into_repo_with(reader, chunk_size, repo_root, None)
}

/// Variant with a per-chunk transform (Phase 2.7: seal each chunk). The hash
/// that names the chunk is of the TRANSFORMED result (ciphertext) — GC and
/// integrity verification work without the key. The manifest's
/// `total_size`/`chunk_size` remain in PLAINTEXT bytes (offset-based reads
/// need them).
/// Per-chunk transform applied before storing (encryption seals each chunk).
pub type ChunkTransform<'a> = &'a dyn Fn(&[u8]) -> Vec<u8>;

pub fn chunk_reader_into_repo_with(
    reader: &mut impl Read,
    chunk_size: u64,
    repo_root: &Path,
    transform: Option<ChunkTransform<'_>>,
) -> std::io::Result<Manifest> {
    let mut hashes = Vec::new();
    let mut total: u64 = 0;
    let mut buf = vec![0u8; chunk_size as usize];
    loop {
        let filled = read_full(reader, &mut buf)?;
        if filled == 0 {
            break;
        }
        let plain = &buf[..filled];
        total += filled as u64;
        let transformed;
        let data: &[u8] = match transform {
            Some(f) => {
                transformed = f(plain);
                &transformed
            }
            None => plain,
        };
        let hash = sha256_hex(data);
        let rel = shard_rel_path(&hash);
        let full = repo_root.join(&rel);
        if !full.exists() {
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // Write via temp + rename: a chunk never shows up truncated to
            // the GC sweep nor to a concurrent reader.
            let tmp = full.with_extension("tmp");
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, &full)?;
        }
        hashes.push(hash);
        if filled < buf.len() {
            break;
        }
    }
    Ok(Manifest {
        total_size: total,
        chunk_size,
        hashes,
    })
}

/// Reads from the reader until `buf` is full or EOF. Returns bytes read.
fn read_full(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            total_size: 250,
            chunk_size: 100,
            hashes: vec![
                sha256_hex(b"a"),
                sha256_hex(b"b"),
                sha256_hex(b"c"),
            ],
        }
    }

    #[test]
    fn manifest_roundtrip_serialize_parse() {
        let m = sample();
        let parsed = Manifest::parse(&m.serialize()).unwrap();
        assert_eq!(parsed, m);
    }

    #[test]
    fn manifest_serialized_bytes_are_detected() {
        assert!(Manifest::is_manifest_bytes(&sample().serialize()));
        assert!(!Manifest::is_manifest_bytes(b"arbitrary content"));
        assert!(!Manifest::is_manifest_bytes(b"drive-sync-chunked v1")); // no \n
        assert!(!Manifest::is_manifest_bytes(b""));
    }

    #[test]
    fn manifest_parse_rejects_unknown_version() {
        let err = Manifest::parse(b"drive-sync-chunked v2\nsize 1\nchunk-size 1\n");
        assert!(err.is_err());
    }

    #[test]
    fn manifest_parse_rejects_missing_fields_and_bad_hash() {
        assert!(Manifest::parse(b"drive-sync-chunked v1\n").is_err());
        assert!(Manifest::parse(b"drive-sync-chunked v1\nsize 10\n").is_err());
        assert!(
            Manifest::parse(b"drive-sync-chunked v1\nsize 10\nchunk-size 10\nsha256 xyz\n")
                .is_err()
        );
    }

    #[test]
    fn manifest_parse_rejects_wrong_hash_count() {
        // size 250 / chunk 100 = 3 chunks, but only 1 hash
        let text = format!(
            "{}\nsize 250\nchunk-size 100\nsha256 {}\n",
            MANIFEST_MAGIC,
            sha256_hex(b"a")
        );
        assert!(Manifest::parse(text.as_bytes()).is_err());
    }

    #[test]
    fn chunks_for_range_maps_offsets() {
        let m = sample(); // 250 bytes in chunks of 100
        assert_eq!(m.chunks_for_range(0, 100), 0..1);
        assert_eq!(m.chunks_for_range(0, 101), 0..2);
        assert_eq!(m.chunks_for_range(99, 2), 0..2);
        assert_eq!(m.chunks_for_range(100, 100), 1..2);
        assert_eq!(m.chunks_for_range(200, 1000), 2..3);
        assert_eq!(m.chunks_for_range(250, 10), 0..0, "past the end");
        assert_eq!(m.chunks_for_range(0, 0), 0..0);
    }

    #[test]
    fn shard_rel_path_uses_two_hex_prefix() {
        let h = sha256_hex(b"x");
        let p = shard_rel_path(&h);
        assert_eq!(p, PathBuf::from(format!(".chunks/{}/{}", &h[..2], h)));
    }

    #[test]
    fn is_chunk_path_detects_reserved_dir() {
        assert!(is_chunk_path(".chunks"));
        assert!(is_chunk_path(".chunks/ab/cdef"));
        assert!(!is_chunk_path("docs/.chunks.md"));
        assert!(!is_chunk_path("regular-file.txt"));
    }

    #[test]
    fn chunk_reader_splits_hashes_and_dedups() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..250u32).map(|i| (i % 251) as u8).collect();

        let m = chunk_reader_into_repo(&mut data.as_slice(), 100, dir.path()).unwrap();
        assert_eq!(m.total_size, 250);
        assert_eq!(m.chunk_size, 100);
        assert_eq!(m.hashes.len(), 3);

        // Chunks on disk, with content and hash matching
        let mut reassembled = Vec::new();
        for h in &m.hashes {
            let bytes = std::fs::read(dir.path().join(shard_rel_path(h))).unwrap();
            assert_eq!(&sha256_hex(&bytes), h);
            reassembled.extend_from_slice(&bytes);
        }
        assert_eq!(reassembled, data);

        // Dedup: re-chunking the same content creates no new files
        let before: Vec<_> = walk_files(dir.path().join(CHUNKS_DIR));
        let m2 = chunk_reader_into_repo(&mut data.as_slice(), 100, dir.path()).unwrap();
        assert_eq!(m2, m);
        assert_eq!(walk_files(dir.path().join(CHUNKS_DIR)), before);
    }

    #[test]
    fn chunk_reader_exact_multiple_has_no_empty_tail() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![7u8; 200];
        let m = chunk_reader_into_repo(&mut data.as_slice(), 100, dir.path()).unwrap();
        assert_eq!(m.hashes.len(), 2, "200/100 = 2 exact chunks, no empty tail");
    }

    fn walk_files(root: PathBuf) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }
}
