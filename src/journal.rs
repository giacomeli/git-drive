//! Write-ahead journal of the sync queue (crash durability).
//!
//! The queue lives in RAM until the debounce drain applies and commits it, so
//! a crash (kill -9, panic, OOM, power cut) used to lose every op enqueued in
//! the last few seconds — including files whose `close(2)` had already
//! returned success to the application (2026-10-13 sandbox run).
//!
//! Every op is persisted here BEFORE it enters the queue: `<seq>.op` holds the
//! header (a JSON array) and `<seq>.data` the content of a write. A record
//! only exists once its `.op` file got its final name — the rename happens
//! under the queue lock, so "seq <= boundary taken by the drain" means exactly
//! "covered by that drain". The drain deletes its records right after the
//! commit (still under the repo lock, so no pull can interleave), and the boot
//! replays whatever is left, in seq order — the arrival order, so the Phase 0
//! causality invariant holds for the replay too. Replaying ops that were
//! already committed is harmless: re-applying the same causal sequence on top
//! of its own result yields the same final state.
//!
//! No fsync: the records survive a process crash (they sit in the page cache);
//! surviving power loss would cost one fsync per close and was not worth it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::sync::{SyncOp, WriteContent};

pub struct Journal {
    dir: PathBuf,
    tmp_counter: AtomicU64,
}

/// Temp files of a record written outside the queue lock, waiting for a seq.
pub struct Prepared {
    op_tmp: PathBuf,
    data_tmp: Option<PathBuf>,
}

fn header(op: &SyncOp) -> String {
    let v: Vec<String> = match op {
        // A base adds `commit, blob` ("" for a path that did not exist).
        SyncOp::Write { path, base, .. } => {
            let mut v = vec!["write".to_string(), path.clone()];
            if let Some(b) = base {
                v.push(b.commit.to_string());
                v.push(b.blob.map(|o| o.to_string()).unwrap_or_default());
            }
            v
        }
        SyncOp::Delete { path } => vec!["delete".into(), path.clone()],
        SyncOp::Mkdir { path } => vec!["mkdir".into(), path.clone()],
        SyncOp::Rmdir { path } => vec!["rmdir".into(), path.clone()],
        SyncOp::Rename { from, to } => vec!["rename".into(), from.clone(), to.clone()],
        SyncOp::Symlink { path, target } => vec!["symlink".into(), path.clone(), target.clone()],
    };
    serde_json::to_string(&v).expect("string array serializes")
}

/// Rebuilds an op from its header. `data` is the content file of a write.
fn parse(header: &str, data: Option<PathBuf>) -> Option<SyncOp> {
    let v: Vec<String> = serde_json::from_str(header.trim()).ok()?;
    let arg = |i: usize| v.get(i).cloned();
    Some(match v.first()?.as_str() {
        "write" => SyncOp::Write {
            path: arg(1)?,
            content: WriteContent::Spooled(data?),
            // Records written before bases existed have none.
            base: arg(2).and_then(|c| git2::Oid::from_str(&c).ok()).map(|commit| {
                crate::sync::WriteBase {
                    commit,
                    blob: arg(3).and_then(|b| git2::Oid::from_str(&b).ok()),
                }
            }),
        },
        "delete" => SyncOp::Delete { path: arg(1)? },
        "mkdir" => SyncOp::Mkdir { path: arg(1)? },
        "rmdir" => SyncOp::Rmdir { path: arg(1)? },
        "rename" => SyncOp::Rename {
            from: arg(1)?,
            to: arg(2)?,
        },
        "symlink" => SyncOp::Symlink {
            path: arg(1)?,
            target: arg(2)?,
        },
        _ => return None,
    })
}

fn seq_name(seq: u64, ext: &str) -> String {
    format!("{:020}.{}", seq, ext)
}

impl Journal {
    /// Opens (creating) the journal directory and loads the records left by a
    /// previous session, sorted by seq. Leftover temp files are discarded:
    /// they never got a seq, so their op never reached the queue.
    pub fn open(dir: &Path) -> std::io::Result<(Journal, Vec<(u64, SyncOp)>)> {
        std::fs::create_dir_all(dir)?;
        let mut records: Vec<(u64, PathBuf)> = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("tmp-") {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            if let Some(stem) = name.strip_suffix(".op") {
                if let Ok(seq) = stem.parse::<u64>() {
                    records.push((seq, path));
                }
            }
        }
        records.sort_by_key(|r| r.0);
        let mut ops = Vec::new();
        for (seq, op_path) in records {
            let data = dir.join(seq_name(seq, "data"));
            let data = data.exists().then_some(data);
            let parsed = std::fs::read_to_string(&op_path)
                .ok()
                .and_then(|h| parse(&h, data));
            match parsed {
                Some(op) => ops.push((seq, op)),
                None => {
                    // A write whose content file is gone was already applied
                    // to the working tree by an interrupted drain (the apply
                    // moves the file) — the tree holds it; drop the record.
                    log::warn!("journal: record {} unusable — dropped", seq);
                    let _ = std::fs::remove_file(&op_path);
                    let _ = std::fs::remove_file(dir.join(seq_name(seq, "data")));
                }
            }
        }
        Ok((
            Journal {
                dir: dir.to_path_buf(),
                tmp_counter: AtomicU64::new(0),
            },
            ops,
        ))
    }

    /// Writes the record's files under temp names (outside the queue lock —
    /// this is where the content bytes hit the disk).
    pub fn prepare(&self, op: &SyncOp) -> std::io::Result<Prepared> {
        let n = self.tmp_counter.fetch_add(1, Ordering::SeqCst);
        let base = format!("tmp-{}-{}", std::process::id(), n);
        let data_tmp = match op {
            SyncOp::Write { content, .. } => {
                let p = self.dir.join(format!("{}.data", base));
                match content {
                    WriteContent::Mem(bytes) => std::fs::write(&p, bytes.as_slice())?,
                    // A copy, not a hard link: the drain renames the spool
                    // file into the working tree, and a later in-place write
                    // there must not alter the journaled bytes. std::fs::copy
                    // clones on APFS/btrfs, so this is cheap where it matters.
                    WriteContent::Spooled(src) => {
                        std::fs::copy(src, &p)?;
                    }
                }
                Some(p)
            }
            _ => None,
        };
        let op_tmp = self.dir.join(format!("{}.op", base));
        if let Err(e) = std::fs::write(&op_tmp, header(op)) {
            if let Some(d) = &data_tmp {
                let _ = std::fs::remove_file(d);
            }
            return Err(e);
        }
        Ok(Prepared { op_tmp, data_tmp })
    }

    /// Gives the prepared record its final seq name (call under the queue
    /// lock, together with the push into the queue). The `.op` rename comes
    /// last: it is what makes the record exist.
    pub fn commit(&self, prepared: Prepared, seq: u64) -> std::io::Result<()> {
        if let Some(d) = prepared.data_tmp {
            std::fs::rename(&d, self.dir.join(seq_name(seq, "data")))?;
        }
        std::fs::rename(&prepared.op_tmp, self.dir.join(seq_name(seq, "op")))
    }

    /// Deletes every record with seq <= `boundary` (covered by a drain that
    /// has committed). The `.op` goes first, so a crash midway never leaves
    /// a header pointing at a deleted content file.
    pub fn truncate_through(&self, boundary: u64) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        let mut data_files = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let Some((stem, ext)) = name.rsplit_once('.') else { continue };
            let Ok(seq) = stem.parse::<u64>() else { continue };
            if seq > boundary {
                continue;
            }
            match ext {
                "op" => {
                    let _ = std::fs::remove_file(&path);
                }
                "data" => data_files.push(path),
                _ => {}
            }
        }
        for d in data_files {
            let _ = std::fs::remove_file(d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn all_ops() -> Vec<SyncOp> {
        vec![
            SyncOp::Write {
                path: "dir/a b\n.txt".into(),
                content: WriteContent::Mem(Arc::new(b"hello".to_vec())),
                base: None,
            },
            SyncOp::Delete { path: "x".into() },
            SyncOp::Mkdir { path: "d".into() },
            SyncOp::Rmdir { path: "d".into() },
            SyncOp::Rename {
                from: "a".into(),
                to: "b".into(),
            },
            SyncOp::Symlink {
                path: "l".into(),
                target: "../t".into(),
            },
        ]
    }

    #[test]
    fn records_roundtrip_in_seq_order_and_temp_files_are_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let (j, left) = Journal::open(dir.path()).unwrap();
        assert!(left.is_empty());
        for (i, op) in all_ops().iter().enumerate() {
            let p = j.prepare(op).unwrap();
            j.commit(p, 10 + i as u64).unwrap();
        }
        // A record that never got its seq (crash between prepare and commit).
        let _orphan = j.prepare(&SyncOp::Delete { path: "never".into() }).unwrap();

        let (_j2, replay) = Journal::open(dir.path()).unwrap();
        assert_eq!(replay.len(), 6, "temp record dropped");
        assert_eq!(replay.iter().map(|r| r.0).collect::<Vec<_>>(), (10..16).collect::<Vec<_>>());
        match &replay[0].1 {
            SyncOp::Write { path, content, .. } => {
                assert_eq!(path, "dir/a b\n.txt", "any path survives the header encoding");
                assert_eq!(content.to_vec(), b"hello");
            }
            other => panic!("expected write, got {:?}", other),
        }
        assert!(matches!(&replay[4].1, SyncOp::Rename { from, to } if from == "a" && to == "b"));
        assert!(matches!(&replay[5].1, SyncOp::Symlink { target, .. } if target == "../t"));
        let leftovers = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("tmp-"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn truncate_removes_only_covered_records() {
        let dir = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(dir.path()).unwrap();
        for seq in 1..=4u64 {
            let op = SyncOp::Write {
                path: format!("f{}", seq),
                content: WriteContent::Mem(Arc::new(vec![seq as u8])),
                base: None,
            };
            let p = j.prepare(&op).unwrap();
            j.commit(p, seq).unwrap();
        }
        j.truncate_through(2);
        let (_j, replay) = Journal::open(dir.path()).unwrap();
        assert_eq!(replay.iter().map(|r| r.0).collect::<Vec<_>>(), vec![3, 4]);
        assert!(!dir.path().join(seq_name(1, "data")).exists());
    }

    #[test]
    fn a_write_keeps_its_base_across_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(dir.path()).unwrap();
        let base = |blob: Option<&str>| crate::sync::WriteBase {
            commit: git2::Oid::from_str("1111111111111111111111111111111111111111").unwrap(),
            blob: blob.map(|b| git2::Oid::from_str(b).unwrap()),
        };
        let edited = base(Some("2222222222222222222222222222222222222222"));
        let created = base(None);
        for (seq, b) in [(1, Some(edited)), (2, Some(created)), (3, None)] {
            let op = SyncOp::Write {
                path: format!("f{}", seq),
                content: WriteContent::Mem(Arc::new(vec![1])),
                base: b,
            };
            let p = j.prepare(&op).unwrap();
            j.commit(p, seq).unwrap();
        }
        let (_j, replay) = Journal::open(dir.path()).unwrap();
        let bases: Vec<_> = replay
            .iter()
            .map(|(_, op)| match op {
                SyncOp::Write { base, .. } => *base,
                other => panic!("{:?}", other),
            })
            .collect();
        assert_eq!(bases, vec![Some(edited), Some(created), None]);
    }

    #[test]
    fn spooled_content_is_copied_not_linked() {
        let dir = tempfile::tempdir().unwrap();
        let spool = dir.path().join("q-1");
        std::fs::write(&spool, b"big").unwrap();
        let jdir = dir.path().join("journal");
        let (j, _) = Journal::open(&jdir).unwrap();
        let op = SyncOp::Write {
            path: "big.bin".into(),
            content: WriteContent::Spooled(spool.clone()),
            base: None,
        };
        let p = j.prepare(&op).unwrap();
        j.commit(p, 1).unwrap();
        // The drain moves the spool file away and may rewrite it in place.
        std::fs::write(&spool, b"changed later").unwrap();
        let (_j, replay) = Journal::open(&jdir).unwrap();
        match &replay[0].1 {
            SyncOp::Write { content, .. } => assert_eq!(content.to_vec(), b"big"),
            other => panic!("{:?}", other),
        }
    }

    #[test]
    fn write_record_without_content_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let (j, _) = Journal::open(dir.path()).unwrap();
        let op = SyncOp::Write {
            path: "a".into(),
            content: WriteContent::Mem(Arc::new(b"x".to_vec())),
            base: None,
        };
        let p = j.prepare(&op).unwrap();
        j.commit(p, 7).unwrap();
        std::fs::remove_file(dir.path().join(seq_name(7, "data"))).unwrap();
        let (_j, replay) = Journal::open(dir.path()).unwrap();
        assert!(replay.is_empty());
        assert!(!dir.path().join(seq_name(7, "op")).exists());
    }
}
