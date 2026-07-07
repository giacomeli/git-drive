use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::{sleep, Instant};
use tokio::task;

use crate::git;
use crate::log_ts;
use crate::notify::{self, Notifier, NotifyEvent};
use crate::status::StatusState;

/// Content of a `SyncOp::Write`. `Mem` shares the same `Arc` as the inode's
/// write buffer (RAM peak ~1x the file size, not one copy per pipeline
/// stage).
#[derive(Debug, Clone)]
pub enum WriteContent {
    Mem(Arc<Vec<u8>>),
    /// Snapshot in a spool file (inside `.git/`, same volume as the working
    /// tree): the drain applies it with `rename`, without materializing in RAM.
    Spooled(PathBuf),
}

impl WriteContent {
    /// Materializes the content (copy). Used by tests and cold paths.
    pub fn to_vec(&self) -> Vec<u8> {
        match self {
            WriteContent::Mem(a) => a.as_ref().clone(),
            WriteContent::Spooled(p) => std::fs::read(p).unwrap_or_default(),
        }
    }

    pub fn len(&self) -> u64 {
        match self {
            WriteContent::Mem(a) => a.len() as u64,
            WriteContent::Spooled(p) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone)]
pub enum SyncOp {
    Write { path: String, content: WriteContent },
    Delete { path: String },
    Mkdir { path: String },
    Rmdir { path: String },
    Rename { from: String, to: String },
    /// Symlink creation with an opaque (text) target. A barrier in the queue,
    /// like every other non-Write op — never coalesces.
    Symlink { path: String, target: String },
}

impl SyncOp {
    pub fn primary_path(&self) -> &str {
        match self {
            SyncOp::Write { path, .. } => path,
            SyncOp::Delete { path } => path,
            SyncOp::Mkdir { path } => path,
            SyncOp::Rmdir { path } => path,
            SyncOp::Rename { to, .. } => to,
            SyncOp::Symlink { path, .. } => path,
        }
    }
}

#[derive(Clone)]
pub struct SyncManager {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    debounce_secs: u64,
    repo_path: PathBuf,
    branch: String,
    user_name: Option<String>,
    user_email: Option<String>,
    remote_name: Option<String>,
    remote_token: Option<String>,
    refresh_flag: Arc<AtomicBool>,
    /// Incremented on every local commit created by the drain. `GitDriveFS`
    /// compares it against the last value seen to discard "clean" write
    /// buffers (content already committed to HEAD) — see `refresh_if_needed`.
    commit_generation: Arc<AtomicU64>,
    /// Serializes the full cycles of both watchers over the same repo: drain
    /// (apply + commit + push + reconcile) and pull (fetch + merge +
    /// checkout) never run concurrently. The atomic unit is the CYCLE, not
    /// the individual git call — a commit landing between the pull's fetch
    /// and checkout was the race window (RF4 of Phase 0).
    repo_lock: Arc<Mutex<()>>,
    /// Observable state (status.json + the `status` subcommand).
    status: Arc<StatusState>,
    /// macOS notifications (starts disabled; the real mount mode enables it).
    notifier: Arc<Notifier>,
    /// Threshold in bytes for converting to manifest + chunks at commit time
    /// (0 = chunking disabled). Post-construction setter, like the status file.
    chunk_threshold_bytes: Arc<AtomicU64>,
    /// Size of each chunk, in bytes.
    chunk_size_bytes: Arc<AtomicU64>,
    /// Encryption at rest (Phase 2.7): `Some` = encrypted repo.
    crypto: Arc<Mutex<Option<Arc<crate::crypt::Crypto>>>>,
    /// Key available ONLY for the migration back to cleartext (config turned
    /// off but the worktree still encrypted — decrypting requires the key).
    migration_crypto: Arc<Mutex<Option<Arc<crate::crypt::Crypto>>>>,
}

struct Inner {
    /// Arrival-order queue. The FUSE kernel order IS the causal order —
    /// preserving it eliminates by construction the "shuffled chained rename"
    /// and "write applied before the directory rename" bugs that the
    /// HashMap + sort-by-op-type combination produced.
    buffer: Vec<SyncOp>,
    last_op: Option<Instant>,
}

impl SyncManager {
    pub fn new(repo_path: &str, debounce_secs: u64) -> Self {
        Self::new_full(
            repo_path,
            "main".to_string(),
            debounce_secs,
            None,
            None,
            None,
            None,
        )
    }

    pub fn new_with_identity(
        repo_path: &str,
        branch: String,
        debounce_secs: u64,
        user_name: Option<String>,
        user_email: Option<String>,
    ) -> Self {
        Self::new_full(
            repo_path,
            branch,
            debounce_secs,
            user_name,
            user_email,
            None,
            None,
        )
    }

    pub fn new_full(
        repo_path: &str,
        branch: String,
        debounce_secs: u64,
        user_name: Option<String>,
        user_email: Option<String>,
        remote_name: Option<String>,
        remote_token: Option<String>,
    ) -> Self {
        Self::new_full_with_pull(
            repo_path,
            branch,
            debounce_secs,
            user_name,
            user_email,
            remote_name,
            remote_token,
            0, // 0 disables the pull watcher
        )
    }

    pub fn new_full_with_pull(
        repo_path: &str,
        branch: String,
        debounce_secs: u64,
        user_name: Option<String>,
        user_email: Option<String>,
        remote_name: Option<String>,
        remote_token: Option<String>,
        pull_interval_secs: u64,
    ) -> Self {
        let status = Arc::new(StatusState::new());
        status.record_boot(repo_path, &branch);
        let manager = SyncManager {
            inner: Arc::new(Mutex::new(Inner {
                buffer: Vec::new(),
                last_op: None,
            })),
            notify: Arc::new(Notify::new()),
            debounce_secs,
            repo_path: PathBuf::from(repo_path),
            branch,
            user_name,
            user_email,
            remote_name,
            remote_token,
            refresh_flag: Arc::new(AtomicBool::new(false)),
            commit_generation: Arc::new(AtomicU64::new(0)),
            repo_lock: Arc::new(Mutex::new(())),
            status,
            notifier: Arc::new(Notifier::new()),
            chunk_threshold_bytes: Arc::new(AtomicU64::new(95 * 1024 * 1024)),
            chunk_size_bytes: Arc::new(AtomicU64::new(80 * 1024 * 1024)),
            crypto: Arc::new(Mutex::new(None)),
            migration_crypto: Arc::new(Mutex::new(None)),
        };

        manager.spawn_watcher();
        if pull_interval_secs > 0 && manager.remote_name.is_some() {
            manager.spawn_pull_watcher(pull_interval_secs);
        }
        manager
    }

    /// Returns the current value of the refresh_flag and clears it. Used by
    /// `GitDriveFS` to decide when to rebuild the indexes.
    pub fn take_refresh_flag(&self) -> bool {
        self.refresh_flag.swap(false, Ordering::SeqCst)
    }

    /// Signals that the FUSE indexes must be rebuilt on the next
    /// lookup/getattr/readdir. Counterpart of `take_refresh_flag`; the
    /// internal watchers use the same mechanism via a clone of the `Arc`.
    pub fn raise_refresh_flag(&self) {
        self.refresh_flag.store(true, Ordering::SeqCst);
    }

    /// Number of local commits created by the drain since boot. Monotonic.
    pub fn commit_generation(&self) -> u64 {
        self.commit_generation.load(Ordering::SeqCst)
    }

    /// Enables status persistence to a file (writes the current state).
    pub fn enable_status_file(&self, path: std::path::PathBuf) {
        self.status.enable_file(path);
    }

    /// Enables encryption at rest (Phase 2.7). Post-construction setter.
    pub fn set_crypto(&self, crypto: Option<Arc<crate::crypt::Crypto>>) {
        *self.crypto.lock() = crypto;
    }

    /// Key for the migration back to cleartext (encryption turned off with
    /// the worktree still encrypted). Ignored when `set_crypto` is active.
    pub fn set_migration_crypto(&self, crypto: Option<Arc<crate::crypt::Crypto>>) {
        *self.migration_crypto.lock() = crypto;
    }

    /// Configures large-file chunking (Phase 2.6). `threshold_bytes` 0
    /// disables it. Post-construction setter so we don't extend the already
    /// long signature of `new_full_with_pull`.
    pub fn set_chunking(&self, threshold_bytes: u64, chunk_size_bytes: u64) {
        self.chunk_threshold_bytes
            .store(threshold_bytes, Ordering::SeqCst);
        self.chunk_size_bytes
            .store(chunk_size_bytes.max(1), Ordering::SeqCst);
    }

    /// Snapshot of the shared fields of the drain cycle.
    fn drain_ctx(&self) -> DrainCtx {
        DrainCtx {
            inner: self.inner.clone(),
            repo_path: self.repo_path.clone(),
            branch: self.branch.clone(),
            user_name: self.user_name.clone(),
            user_email: self.user_email.clone(),
            remote_name: self.remote_name.clone(),
            remote_token: self.remote_token.clone(),
            refresh_flag: self.refresh_flag.clone(),
            commit_generation: self.commit_generation.clone(),
            repo_lock: self.repo_lock.clone(),
            status: self.status.clone(),
            notifier: self.notifier.clone(),
            chunk_threshold_bytes: self.chunk_threshold_bytes.clone(),
            chunk_size_bytes: self.chunk_size_bytes.clone(),
            crypto: self.crypto.clone(),
            migration_crypto: self.migration_crypto.clone(),
        }
    }

    /// Drains the queue NOW, synchronously (apply + commit + push, under the
    /// `repo_lock`). Used during clean shutdown: after the unmount, it
    /// guarantees no queued op is lost when the process dies.
    pub fn drain_now(&self) {
        let changes = { std::mem::take(&mut self.inner.lock().buffer) };
        if changes.is_empty() {
            return;
        }
        println!(
            "[{}] drain_now: draining {} pending op(s) at shutdown",
            log_ts(),
            changes.len()
        );
        self.drain_ctx().run(changes);
    }

    /// Access to the observable state (snapshot for tests/embedded CLI).
    pub fn status_state(&self) -> Arc<StatusState> {
        self.status.clone()
    }

    /// Access to the notifier (main uses it for mount/stop events).
    pub fn notifier(&self) -> Arc<Notifier> {
        self.notifier.clone()
    }

    /// Enables macOS notifications (real mount mode).
    pub fn enable_notifications(&self) {
        self.notifier.enable();
    }

    fn spawn_pull_watcher(&self, interval_secs: u64) {
        let inner = self.inner.clone();
        let repo_path = self.repo_path.clone();
        let branch = self.branch.clone();
        let remote_name = self.remote_name.clone();
        let remote_token = self.remote_token.clone();
        let user_name = self.user_name.clone();
        let user_email = self.user_email.clone();
        let refresh_flag = self.refresh_flag.clone();
        let repo_lock = self.repo_lock.clone();
        let status = self.status.clone();
        let notifier = self.notifier.clone();

        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(interval_secs)).await;

                // Skip the pull while a batch is pending (avoids racing the local commit)
                let pending = { !inner.lock().buffer.is_empty() };
                if pending {
                    status.record_pull("skipped: sync queue pending");
                    continue;
                }
                let Some(ref remote) = remote_name else {
                    break;
                };

                // Serialize with the drain cycle (RF4). No .await while the
                // guard is alive — the lock only covers blocking git calls.
                let _repo_guard = repo_lock.lock();

                let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("[{}] pull: error opening repo: {}", log_ts(), e);
                        continue;
                    }
                };

                // RF6 guard: merge/checkout force only with a clean working
                // tree. A dirty tree = local changes not yet committed
                // (typical of the identity-less mode) — a fast-forward would
                // destroy them.
                match git::is_working_tree_clean(&repo) {
                    Ok(true) => {
                        // Blocking condition resolved — re-arm the notification.
                        notifier.reset(notify::KEY_PULL_BLOCKED);
                    }
                    Ok(false) => {
                        println!(
                            "[{}] pull: working tree has uncommitted changes — skipping",
                            log_ts()
                        );
                        status.record_pull("skipped: dirty working tree");
                        notifier.notify(NotifyEvent::PullBlocked);
                        continue;
                    }
                    Err(e) => {
                        eprintln!("[{}] pull: error checking working tree: {}", log_ts(), e);
                        status.record_pull("error checking working tree");
                        continue;
                    }
                }

                if let Err(e) =
                    git::fetch_remote(&repo, &branch, remote, remote_token.as_deref())
                {
                    eprintln!("[{}] pull: fetch error: {}", log_ts(), e);
                    status.record_pull("fetch error");
                    continue;
                }

                // With identity: 3-way merge + conflicted copy on conflict.
                // Without identity: degrades to FF-only (no merge commit possible).
                let outcome: Result<git::MergeOutcome, git2::Error> =
                    match (user_name.as_deref(), user_email.as_deref()) {
                        (Some(name), Some(email)) => {
                            let suffix = conflict_suffix();
                            git::pull_and_merge(&repo, &branch, remote, name, email, &suffix)
                        }
                        _ => git::merge_ff_only(&repo, &branch).map(|applied| {
                            if applied {
                                git::MergeOutcome::FastForwarded
                            } else {
                                git::MergeOutcome::NoOp
                            }
                        }),
                    };

                match outcome {
                    Ok(git::MergeOutcome::NoOp) => {
                        status.record_pull("up to date");
                    }
                    Ok(git::MergeOutcome::FastForwarded) => {
                        println!("[{}] pull: fast-forward applied", log_ts());
                        status.record_pull("fast-forward");
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Ok(git::MergeOutcome::Merged) => {
                        println!("[{}] pull: 3-way merge, no conflicts", log_ts());
                        status.record_pull("3-way merge");
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Ok(git::MergeOutcome::Conflicted { resolved, suffix }) => {
                        println!(
                            "[{}] pull: resolved {} conflict(s) — local versions at *.{}: {:?}",
                            log_ts(),
                            resolved.len(),
                            suffix,
                            resolved
                        );
                        status.record_pull(&format!("merge with {} conflict(s)", resolved.len()));
                        let copies: Vec<String> = resolved
                            .iter()
                            .map(|p| format!("{}.{}", p, suffix))
                            .collect();
                        status.add_conflicts(copies.clone());
                        notifier.notify(NotifyEvent::Conflicts { copies });
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Err(e) => {
                        eprintln!("[{}] pull: merge error: {}", log_ts(), e);
                        status.record_pull("merge error");
                    }
                }
            }
        });
    }

    pub fn queue_write(&self, path: &str, content: Vec<u8>) {
        self.queue_write_content(path, WriteContent::Mem(Arc::new(content)));
    }

    /// Enqueues a write with already-packaged content (e.g. an `Arc` shared
    /// with the FUSE write buffer — no copy on the hot path).
    pub fn queue_write_content(&self, path: &str, content: WriteContent) {
        self.enqueue(SyncOp::Write {
            path: path.to_string(),
            content,
        });
    }

    pub fn queue_delete(&self, path: &str) {
        self.enqueue(SyncOp::Delete {
            path: path.to_string(),
        });
    }

    pub fn queue_mkdir(&self, path: &str) {
        self.enqueue(SyncOp::Mkdir {
            path: path.to_string(),
        });
    }

    pub fn queue_rmdir(&self, path: &str) {
        self.enqueue(SyncOp::Rmdir {
            path: path.to_string(),
        });
    }

    pub fn queue_rename(&self, from: &str, to: &str) {
        self.enqueue(SyncOp::Rename {
            from: from.to_string(),
            to: to.to_string(),
        });
    }

    pub fn queue_symlink(&self, path: &str, target: &str) {
        self.enqueue(SyncOp::Symlink {
            path: path.to_string(),
            target: target.to_string(),
        });
    }

    fn enqueue(&self, op: SyncOp) {
        if op_touches_ephemeral(&op) {
            return;
        }
        {
            let mut inner = self.inner.lock();
            push_coalescing(&mut inner.buffer, op);
            inner.last_op = Some(Instant::now());
        }
        self.notify.notify_one();
    }

    pub fn buffer_snapshot(&self) -> Vec<SyncOp> {
        self.inner.lock().buffer.clone()
    }

    /// Cheap check (no content cloning) for a non-empty queue. Used as a
    /// guard on the FUSE hot path (`refresh_if_needed`).
    pub fn has_pending_ops(&self) -> bool {
        !self.inner.lock().buffer.is_empty()
    }

    fn spawn_watcher(&self) {
        let debounce_secs = self.debounce_secs;
        let notify = self.notify.clone();
        let ctx = Arc::new(self.drain_ctx());

        task::spawn(async move {
            loop {
                notify.notified().await;

                loop {
                    sleep(Duration::from_secs(debounce_secs)).await;

                    let drained = {
                        let mut inner = ctx.inner.lock();
                        let should_drain = inner
                            .last_op
                            .map(|l| l.elapsed() >= Duration::from_secs(debounce_secs))
                            .unwrap_or(false)
                            && !inner.buffer.is_empty();
                        if !should_drain {
                            None
                        } else {
                            Some(std::mem::take(&mut inner.buffer))
                        }
                    };

                    if let Some(changes) = drained {
                        println!("[{}] drain: syncing changes", log_ts());
                        ctx.run(changes);
                        break;
                    }
                }
            }
        });
    }
}

/// Shared context of the drain cycle — the debounce watcher and the
/// shutdown's `drain_now` run exactly the same body.
struct DrainCtx {
    inner: Arc<Mutex<Inner>>,
    repo_path: PathBuf,
    branch: String,
    user_name: Option<String>,
    user_email: Option<String>,
    remote_name: Option<String>,
    remote_token: Option<String>,
    refresh_flag: Arc<AtomicBool>,
    commit_generation: Arc<AtomicU64>,
    repo_lock: Arc<Mutex<()>>,
    status: Arc<StatusState>,
    notifier: Arc<Notifier>,
    chunk_threshold_bytes: Arc<AtomicU64>,
    chunk_size_bytes: Arc<AtomicU64>,
    crypto: Arc<Mutex<Option<Arc<crate::crypt::Crypto>>>>,
    migration_crypto: Arc<Mutex<Option<Arc<crate::crypt::Crypto>>>>,
}

impl DrainCtx {
    fn run(&self, changes: Vec<SyncOp>) {
        // Serialize the whole cycle (apply + commit + push + reconcile) with
        // the pull watcher (RF4 of Phase 0). No .await in the guard's scope.
        let _repo_guard = self.repo_lock.lock();
        let crypto = self.crypto.lock().clone();
        let migration_crypto = self.migration_crypto.lock().clone();
        let chunk_threshold = self.chunk_threshold_bytes.load(Ordering::SeqCst);
        let chunk_size = self.chunk_size_bytes.load(Ordering::SeqCst).max(1);
        // Apply in arrival order — the queue is already causal. With
        // encryption, the cleartext->encrypted translation of paths and
        // content happens HERE (the FUSE layer and the queue work 100% in
        // cleartext).
        let ops: Vec<&SyncOp> = changes.iter().collect();
        for op in &ops {
            apply_op_to_disk(&self.repo_path, op, crypto.as_deref(), chunk_threshold);
        }
        // Chunking (Phase 2.6): the sweep converts ANY raw file above the
        // threshold — with encryption, each chunk is sealed and the hash is
        // over the ciphertext. Invariant: no raw blob above the threshold is
        // staged. Afterwards, the state migrations (Phase 2.7) conform the
        // worktree to the config; the GC removes orphan chunks last.
        let mut converted = 0usize;
        if chunk_threshold > 0 {
            converted += convert_oversized_worktree_files(
                &self.repo_path,
                chunk_threshold,
                chunk_size,
                crypto.as_deref(),
            );
        }
        if let Some(c) = crypto.as_deref() {
            let migrated = migrate_worktree_to_encrypted(&self.repo_path, c);
            if migrated > 0 {
                println!(
                    "[{}] crypto: {} item(s) migrated to encrypted. WARNING: the PREVIOUS Git history remains in cleartext on the remote — for a public repo, prefer starting from a fresh repo",
                    log_ts(),
                    migrated
                );
            }
            converted += migrated;
        } else if let Some(mk) = migration_crypto.as_deref() {
            let migrated = migrate_worktree_to_clear(&self.repo_path, mk);
            if migrated > 0 {
                println!(
                    "[{}] crypto: {} item(s) decrypted back to cleartext",
                    log_ts(),
                    migrated
                );
                if chunk_threshold > 0 {
                    converted += convert_oversized_worktree_files(
                        &self.repo_path,
                        chunk_threshold,
                        chunk_size,
                        None,
                    );
                }
            }
            converted += migrated;
        }
        if chunk_threshold > 0 {
            gc_orphan_chunks(
                &self.repo_path,
                crypto.as_deref().or(migration_crypto.as_deref()),
            );
        }
        let commit_oid = commit_batch(
            &self.repo_path,
            &self.branch,
            &self.user_name,
            &self.user_email,
            &ops,
        );
        let mut pushed: Option<bool> = None;
        if let Some(_oid) = commit_oid {
            self.commit_generation.fetch_add(1, Ordering::SeqCst);
            // The sweep's conversion creates new paths in HEAD that the FUSE
            // index may not know about (file arrived from outside the mount)
            // — without the refresh, the file would stay invisible in the
            // drive until a real pull (zip migration incident, 2026-07-03).
            // Flag AFTER the commit: a rebuild before it would read the old
            // HEAD and consume the flag for nothing.
            if converted > 0 {
                self.refresh_flag.store(true, Ordering::SeqCst);
            }
            if let Some(ref remote) = self.remote_name {
                let ok = push_batch(&self.repo_path, &self.branch, remote, &self.remote_token);
                if ok {
                    pushed = Some(true);
                } else {
                    let recovered = reconcile_and_retry_push(
                        &self.repo_path,
                        &self.branch,
                        remote,
                        &self.remote_token,
                        &self.user_name,
                        &self.user_email,
                        &self.status,
                        &self.notifier,
                    );
                    pushed = Some(recovered);
                    if recovered {
                        self.refresh_flag.store(true, Ordering::SeqCst);
                    } else {
                        // Push + reconcile failed: local commit left unpublished.
                        self.notifier.notify(NotifyEvent::PushFailed);
                    }
                }
            }
        }
        let pending = self.inner.lock().buffer.len();
        self.status.record_drain(
            ops.len(),
            commit_oid.map(|o| o.to_string()),
            pushed,
            pending,
        );
    }
}

/// Translates a CLEARTEXT path from the queue into the on-disk path
/// (encrypted when encryption is active). A name that exceeds the filesystem
/// limit falls back to cleartext with a logged error — better than losing
/// the op.
fn disk_rel_path(crypto: Option<&crate::crypt::Crypto>, clear: &str) -> String {
    match crypto {
        Some(c) => match c.enc_path(clear) {
            Ok(enc) => enc,
            Err(e) => {
                eprintln!("[{}] crypto: {} at {:?} — keeping cleartext", log_ts(), e, clear);
                clear.to_string()
            }
        },
        None => clear.to_string(),
    }
}

fn apply_op_to_disk(
    repo_path: &Path,
    op: &SyncOp,
    crypto: Option<&crate::crypt::Crypto>,
    chunk_threshold: u64,
) {
    match op {
        SyncOp::Write { path, content } => {
            let full = repo_path.join(disk_rel_path(crypto, path));
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let result = match (content, crypto) {
                (WriteContent::Mem(data), None) => std::fs::write(&full, data.as_slice()),
                (WriteContent::Mem(data), Some(c)) => {
                    if chunk_threshold > 0 && data.len() as u64 > chunk_threshold {
                        // Above the threshold: temporary plaintext at the encrypted
                        // path — the chunking sweep seals chunk by chunk right after
                        // (sealing it whole here would materialize everything in RAM).
                        std::fs::write(&full, data.as_slice())
                    } else {
                        std::fs::write(&full, c.seal(data.as_slice()))
                    }
                }
                // rename moves the snapshot without going through RAM; falls
                // back to copy+remove if the spool is on another volume.
                (WriteContent::Spooled(src), None) => {
                    std::fs::rename(src, &full).or_else(|_| {
                        std::fs::copy(src, &full).map(|_| ())?;
                        let _ = std::fs::remove_file(src);
                        Ok(())
                    })
                }
                (WriteContent::Spooled(src), Some(c)) => {
                    let len = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
                    if chunk_threshold > 0 && len > chunk_threshold {
                        // Same rationale as the large Mem case: the sweep seals per chunk.
                        std::fs::rename(src, &full).or_else(|_| {
                            std::fs::copy(src, &full).map(|_| ())?;
                            let _ = std::fs::remove_file(src);
                            Ok(())
                        })
                    } else {
                        std::fs::read(src).and_then(|plain| {
                            let _ = std::fs::remove_file(src);
                            std::fs::write(&full, c.seal(&plain))
                        })
                    }
                }
            };
            match result {
                Ok(_) => println!("[{}] disk: write {}", log_ts(), path),
                Err(e) => eprintln!("[{}] disk: error saving {}: {}", log_ts(), path, e),
            }
        }
        SyncOp::Delete { path } => {
            let full = repo_path.join(disk_rel_path(crypto, path));
            let result = match std::fs::remove_file(&full) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && crypto.is_some() => {
                    // Mixed state: the on-disk version may still be in cleartext.
                    std::fs::remove_file(repo_path.join(path))
                }
                other => other,
            };
            match result {
                Ok(_) => println!("[{}] disk: unlink {}", log_ts(), path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // The path never reached the disk (e.g. `.sb-*` temp dirs
                    // that the filter blocks on creation but lets through on
                    // delete). Idempotent: already gone = successful removal.
                }
                Err(e) => eprintln!("[{}] disk: error removing {}: {}", log_ts(), path, e),
            }
        }
        SyncOp::Mkdir { path } => {
            let full = repo_path.join(disk_rel_path(crypto, path));
            if let Err(e) = std::fs::create_dir_all(&full) {
                eprintln!("[{}] disk: error creating directory {}: {}", log_ts(), path, e);
                return;
            }
            let keep_rel = disk_rel_path(crypto, &format!("{}/.gitkeep", path));
            let keep = repo_path.join(keep_rel);
            if !keep.exists() {
                let keep_bytes: Vec<u8> = match crypto {
                    Some(c) => c.seal(b""),
                    None => Vec::new(),
                };
                if let Err(e) = std::fs::write(&keep, keep_bytes) {
                    eprintln!(
                        "[{}] disk: error creating .gitkeep in {}: {}",
                        log_ts(),
                        path,
                        e
                    );
                    return;
                }
            }
            println!("[{}] disk: mkdir {}", log_ts(), path);
        }
        SyncOp::Rmdir { path } => {
            let full = repo_path.join(disk_rel_path(crypto, path));
            let keep = repo_path.join(disk_rel_path(crypto, &format!("{}/.gitkeep", path)));
            let _ = std::fs::remove_file(&keep);
            match std::fs::remove_dir(&full) {
                Ok(_) => println!("[{}] disk: rmdir {}", log_ts(), path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Same rationale as Delete: a directory that never reached
                    // the disk (e.g. `.sb-*`) yields ENOENT on rmdir. Silence it.
                }
                Err(e) => eprintln!(
                    "[{}] disk: error removing directory {}: {}",
                    log_ts(),
                    path,
                    e
                ),
            }
        }
        SyncOp::Rename { from, to } => {
            let mut from_full = repo_path.join(disk_rel_path(crypto, from));
            if crypto.is_some() && !from_full.exists() {
                // Mixed state: the source may still have its cleartext name.
                from_full = repo_path.join(from);
            }
            let to_full = repo_path.join(disk_rel_path(crypto, to));
            if let Some(parent) = to_full.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            match std::fs::rename(&from_full, &to_full) {
                Ok(_) => println!("[{}] disk: rename {} -> {}", log_ts(), from, to),
                Err(e) => eprintln!(
                    "[{}] disk: error renaming {} -> {}: {}",
                    log_ts(),
                    from,
                    to,
                    e
                ),
            }
        }
        SyncOp::Symlink { path, target } => {
            let full = repo_path.join(disk_rel_path(crypto, path));
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let disk_target = disk_rel_path(crypto, target);
            // symlink(2) fails with EEXIST if the path exists — removing it
            // first reproduces the queue semantics (the final state is the
            // symlink).
            let _ = std::fs::remove_file(&full);
            match std::os::unix::fs::symlink(&disk_target, &full) {
                Ok(_) => println!("[{}] disk: symlink {} -> {}", log_ts(), path, target),
                Err(e) => eprintln!(
                    "[{}] disk: error creating symlink {} -> {}: {}",
                    log_ts(),
                    path,
                    target,
                    e
                ),
            }
        }
    }
}

/// Returns the oid of the created commit, or `None` (no identity, clean tree
/// or error — the reason goes to the log).
fn commit_batch(
    repo_path: &Path,
    branch: &str,
    user_name: &Option<String>,
    user_email: &Option<String>,
    ops: &[&SyncOp],
) -> Option<git2::Oid> {
    let (Some(name), Some(email)) = (user_name.as_deref(), user_email.as_deref()) else {
        println!(
            "[{}] commit: user_name/user_email missing — skipping commit",
            log_ts()
        );
        return None;
    };
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] commit: error opening repo: {}", log_ts(), e);
            return None;
        }
    };
    let message = build_commit_message(ops);
    match git::commit_working_tree(&repo, branch, name, email, &message) {
        Ok(Some(oid)) => {
            println!("[{}] commit: created {}", log_ts(), oid);
            Some(oid)
        }
        Ok(None) => {
            println!("[{}] commit: clean tree, nothing to commit", log_ts());
            None
        }
        Err(e) => {
            eprintln!("[{}] commit: error: {}", log_ts(), e);
            None
        }
    }
}

fn push_batch(repo_path: &Path, branch: &str, remote: &str, token: &Option<String>) -> bool {
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] push: error opening repo: {}", log_ts(), e);
            return false;
        }
    };
    match git::push_to_remote(&repo, branch, remote, token.as_deref()) {
        Ok(_) => {
            println!("[{}] push: {} ok", log_ts(), remote);
            true
        }
        Err(e) => {
            eprintln!("[{}] push: error on {}: {}", log_ts(), remote, e);
            false
        }
    }
}

/// The push failed (usually non-FF). Does fetch + pull_and_merge + a single push
/// retry. Returns `true` if the retry succeeded (implies the working tree changed
/// and the caller must signal a refresh to the FUSE layer).
#[allow(clippy::too_many_arguments)]
fn reconcile_and_retry_push(
    repo_path: &Path,
    branch: &str,
    remote: &str,
    token: &Option<String>,
    user_name: &Option<String>,
    user_email: &Option<String>,
    status: &StatusState,
    notifier: &Notifier,
) -> bool {
    let (Some(name), Some(email)) = (user_name.as_deref(), user_email.as_deref()) else {
        eprintln!(
            "[{}] reconcile: push failed and no identity configured — cannot reconcile",
            log_ts()
        );
        return false;
    };
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] reconcile: error opening repo: {}", log_ts(), e);
            return false;
        }
    };

    if let Err(e) = git::fetch_remote(&repo, branch, remote, token.as_deref()) {
        eprintln!("[{}] reconcile: fetch error: {}", log_ts(), e);
        return false;
    }

    let suffix = conflict_suffix();
    match git::pull_and_merge(&repo, branch, remote, name, email, &suffix) {
        Ok(git::MergeOutcome::NoOp) => {
            eprintln!(
                "[{}] reconcile: no divergence — the original error may be network/auth",
                log_ts()
            );
            return false;
        }
        Ok(git::MergeOutcome::Conflicted { ref resolved, ref suffix }) => {
            println!(
                "[{}] reconcile: {} conflict(s) — local versions at *.{}: {:?}",
                log_ts(),
                resolved.len(),
                suffix,
                resolved
            );
            let copies: Vec<String> = resolved
                .iter()
                .map(|p| format!("{}.{}", p, suffix))
                .collect();
            status.add_conflicts(copies.clone());
            notifier.notify(NotifyEvent::Conflicts { copies });
        }
        Ok(o) => {
            println!("[{}] reconcile: applied ({:?})", log_ts(), o);
        }
        Err(e) => {
            eprintln!("[{}] reconcile: pull_and_merge error: {}", log_ts(), e);
            return false;
        }
    }

    match git::push_to_remote(&repo, branch, remote, token.as_deref()) {
        Ok(_) => {
            println!("[{}] push: retry on {} ok", log_ts(), remote);
            true
        }
        Err(e) => {
            eprintln!("[{}] push: retry failed again: {}", log_ts(), e);
            false
        }
    }
}

/// Generates a unique suffix for conflicted copies. Format: `conflict-<hostname>-<unix_ts>`.
fn conflict_suffix() -> String {
    let host = gethostname_or("unknown");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("conflict-{}-{}", host, ts)
}

fn gethostname_or(fallback: &str) -> String {
    let mut buf = [0u8; 256];
    let ret = unsafe {
        libc::gethostname(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len() as libc::size_t,
        )
    };
    if ret != 0 {
        return fallback.to_string();
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => fallback.to_string(),
    }
}

fn build_commit_message(ops: &[&SyncOp]) -> String {
    let n = ops.len();
    let paths: Vec<&str> = ops.iter().map(|op| op.primary_path()).collect();
    let preview: Vec<&str> = paths.iter().take(3).copied().collect();
    let more = if n > preview.len() {
        format!(", +{} more", n - preview.len())
    } else {
        String::new()
    };
    format!("drive-sync: {} changes ({}{})", n, preview.join(", "), more)
}

/// Sweeps the working tree and converts into manifest + chunks every regular
/// file above `threshold` that is neither ignored nor already a manifest.
/// Skips `.git` (at any level) and `.chunks`. Streaming: RAM bounded by one
/// chunk.
fn convert_oversized_worktree_files(
    repo_path: &Path,
    threshold: u64,
    chunk_size: u64,
    crypto: Option<&crate::crypt::Crypto>,
) -> usize {
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] chunk: error opening repo: {}", log_ts(), e);
            return 0;
        }
    };
    let mut converted = 0usize;
    for (full, rel) in walk_worktree_files(repo_path) {
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        if !meta.is_file() || meta.len() <= threshold {
            continue;
        }
        // Ignored files are local-only: they never get committed, and
        // chunking them would break reads. In encrypted mode the ignore
        // semantics are disabled.
        if crypto.is_none() && repo.is_path_ignored(&rel).unwrap_or(false) {
            continue;
        }
        if file_starts_with_manifest_magic(&full) {
            continue;
        }
        if file_starts_with_encrypted_magic(&full) {
            // Sealed whole above the threshold: legacy the current flow no
            // longer produces (large writes arrive as plaintext for this sweep).
            eprintln!(
                "[{}] chunk: {} sealed above the threshold — skipped (re-save the file)",
                log_ts(),
                rel
            );
            continue;
        }
        match convert_file_to_chunks(repo_path, &full, chunk_size, crypto) {
            Ok(manifest) => {
                converted += 1;
                println!(
                    "[{}] chunk: {} -> {} chunk(s) of up to {} bytes",
                    log_ts(),
                    rel,
                    manifest.hashes.len(),
                    chunk_size
                );
            }
            Err(e) => eprintln!("[{}] chunk: error converting {}: {}", log_ts(), rel, e),
        }
    }
    converted
}

/// Converts a raw file into chunks + manifest (replaces the file with the
/// manifest via temp + rename).
fn convert_file_to_chunks(
    repo_root: &Path,
    full: &Path,
    chunk_size: u64,
    crypto: Option<&crate::crypt::Crypto>,
) -> std::io::Result<crate::chunk::Manifest> {
    let mut reader = std::fs::File::open(full)?;
    let seal_transform = crypto.map(|c| move |plain: &[u8]| c.seal(plain));
    let manifest = crate::chunk::chunk_reader_into_repo_with(
        &mut reader,
        chunk_size,
        repo_root,
        seal_transform
            .as_ref()
            .map(|f| f as &dyn Fn(&[u8]) -> Vec<u8>),
    )?;
    // The manifest stays plaintext here; the state migration (right after,
    // in the same drain) seals it and moves it to the encrypted name when
    // encryption is active.
    let tmp = full.with_extension("drive-sync-manifest.tmp");
    std::fs::write(&tmp, manifest.serialize())?;
    std::fs::rename(&tmp, full)?;
    Ok(manifest)
}

/// `true` if the file starts with the encrypted-content magic.
fn file_starts_with_encrypted_magic(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 32];
    let Ok(len) = f.read(&mut buf) else { return false };
    crate::crypt::Crypto::is_encrypted_bytes(&buf[..len])
}

/// `cleartext -> encrypted` migration: conforms every file/symlink in the
/// working tree to the encrypted state (name AND content). Idempotent — a
/// file already conforming is skipped. Returns how many items were converted.
fn migrate_worktree_to_encrypted(repo_path: &Path, c: &crate::crypt::Crypto) -> usize {
    let mut converted = 0usize;
    for (full, rel) in walk_worktree_files(repo_path) {
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        let name_ok = rel
            .split('/')
            .all(|comp| c.dec_component(comp).is_some());
        if meta.file_type().is_symlink() {
            let Ok(target) = std::fs::read_link(&full) else { continue };
            let target_str = target.to_string_lossy().into_owned();
            let target_ok = target_str
                .split('/')
                .all(|comp| matches!(comp, "" | "." | "..") || c.dec_component(comp).is_some());
            if name_ok && target_ok {
                continue;
            }
            let clear_rel = c.dec_path(&rel);
            let clear_target = c.dec_path(&target_str);
            let (Ok(new_rel), Ok(new_target)) =
                (c.enc_path(&clear_rel), c.enc_path(&clear_target))
            else {
                eprintln!("[{}] crypto: name too long at {:?} — skipped", log_ts(), clear_rel);
                continue;
            };
            let new_full = repo_path.join(&new_rel);
            if let Some(parent) = new_full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(&full);
            let _ = std::fs::remove_file(&new_full);
            if std::os::unix::fs::symlink(&new_target, &new_full).is_ok() {
                converted += 1;
            }
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let sealed = file_starts_with_encrypted_magic(&full);
        if name_ok && sealed {
            continue;
        }
        let clear_rel = c.dec_path(&rel);
        let Ok(new_rel) = c.enc_path(&clear_rel) else {
            eprintln!("[{}] crypto: name too long at {:?} — skipped", log_ts(), clear_rel);
            continue;
        };
        let new_full = repo_path.join(&new_rel);
        if let Some(parent) = new_full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if sealed {
            // Content is fine; only the name needs to migrate.
            if new_full != full && std::fs::rename(&full, &new_full).is_ok() {
                converted += 1;
            }
            continue;
        }
        let Ok(plain) = std::fs::read(&full) else { continue };
        if crate::chunk::Manifest::is_manifest_bytes(&plain) {
            // File chunked IN CLEARTEXT: sealing only the manifest would
            // leave the old chunks in plaintext in the repo (a leak). Expand
            // to a temp file (streaming) and re-chunk with sealing; the
            // cleartext chunks become orphans and the GC (which runs later,
            // with the key) removes them.
            if let Ok(old_manifest) = crate::chunk::Manifest::parse(&plain) {
                let any_clear_chunk = old_manifest.hashes.iter().any(|h| {
                    !file_starts_with_encrypted_magic(
                        &repo_path.join(crate::chunk::shard_rel_path(h)),
                    )
                });
                if !any_clear_chunk {
                    // Chunks already sealed (new write under encryption):
                    // sealing the manifest is enough — falls through to the
                    // common path below.
                } else {
                let tmp = new_full.with_extension("drive-sync-rechunk.tmp");
                let expand = (|| -> std::io::Result<()> {
                    use std::io::Write;
                    let mut out = std::fs::File::create(&tmp)?;
                    for h in &old_manifest.hashes {
                        let chunk =
                            std::fs::read(repo_path.join(crate::chunk::shard_rel_path(h)))?;
                        // Mixed state: a sealed chunk is opened; cleartext passes through raw.
                        let bytes = if crate::crypt::Crypto::is_encrypted_bytes(&chunk) {
                            c.open(&chunk)
                                .map_err(|e| std::io::Error::other(e.to_string()))?
                        } else {
                            chunk
                        };
                        out.write_all(&bytes)?;
                    }
                    Ok(())
                })();
                if expand.is_err() {
                    let _ = std::fs::remove_file(&tmp);
                    eprintln!(
                        "[{}] crypto: failed to expand cleartext manifest {:?} — skipped",
                        log_ts(),
                        rel
                    );
                    continue;
                }
                let seal = |p: &[u8]| c.seal(p);
                let rechunk = std::fs::File::open(&tmp).and_then(|mut r| {
                    crate::chunk::chunk_reader_into_repo_with(
                        &mut r,
                        old_manifest.chunk_size,
                        repo_path,
                        Some(&seal as &dyn Fn(&[u8]) -> Vec<u8>),
                    )
                });
                let _ = std::fs::remove_file(&tmp);
                if let Ok(new_manifest) = rechunk {
                    let tmp2 = new_full.with_extension("drive-sync-crypt.tmp");
                    if std::fs::write(&tmp2, c.seal(&new_manifest.serialize())).is_ok()
                        && std::fs::rename(&tmp2, &new_full).is_ok()
                    {
                        if new_full != full {
                            let _ = std::fs::remove_file(&full);
                        }
                        converted += 1;
                    }
                }
                continue;
                }
            }
        }
        let tmp = new_full.with_extension("drive-sync-crypt.tmp");
        if std::fs::write(&tmp, c.seal(&plain)).is_ok()
            && std::fs::rename(&tmp, &new_full).is_ok()
        {
            if new_full != full {
                let _ = std::fs::remove_file(&full);
            }
            converted += 1;
        }
    }
    prune_empty_dirs(repo_path);
    converted
}

/// `encrypted -> cleartext` migration: decrypts names and content; sealed
/// manifests are expanded back into the whole file (the chunk sweep
/// re-chunks it in cleartext right after, if still large).
fn migrate_worktree_to_clear(repo_path: &Path, mk: &crate::crypt::Crypto) -> usize {
    let mut converted = 0usize;
    for (full, rel) in walk_worktree_files(repo_path) {
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        let clear_rel = mk.dec_path(&rel);
        let name_cipher = clear_rel != rel;
        if meta.file_type().is_symlink() {
            let Ok(target) = std::fs::read_link(&full) else { continue };
            let clear_target = mk.dec_path(&target.to_string_lossy());
            let target_cipher = clear_target != target.to_string_lossy();
            if !name_cipher && !target_cipher {
                continue;
            }
            let new_full = repo_path.join(&clear_rel);
            if let Some(parent) = new_full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(&full);
            let _ = std::fs::remove_file(&new_full);
            if std::os::unix::fs::symlink(&clear_target, &new_full).is_ok() {
                converted += 1;
            }
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let sealed = file_starts_with_encrypted_magic(&full);
        if !sealed && !name_cipher {
            continue;
        }
        let new_full = repo_path.join(&clear_rel);
        if let Some(parent) = new_full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if !sealed {
            // Only the name is encrypted.
            if std::fs::rename(&full, &new_full).is_ok() {
                converted += 1;
            }
            continue;
        }
        let Ok(sealed_bytes) = std::fs::read(&full) else { continue };
        let Ok(plain) = mk.open(&sealed_bytes) else {
            eprintln!(
                "[{}] crypto: failed to decrypt {:?} during migration — skipped",
                log_ts(),
                rel
            );
            continue;
        };
        if crate::chunk::Manifest::is_manifest_bytes(&plain) {
            // Expand: reassembles the whole file by decrypting chunk by
            // chunk (streaming to disk; RAM bounded by one chunk).
            let Ok(manifest) = crate::chunk::Manifest::parse(&plain) else { continue };
            let tmp = new_full.with_extension("drive-sync-expand.tmp");
            let ok = (|| -> std::io::Result<()> {
                use std::io::Write;
                let mut out = std::fs::File::create(&tmp)?;
                for h in &manifest.hashes {
                    let sealed_chunk =
                        std::fs::read(repo_path.join(crate::chunk::shard_rel_path(h)))?;
                    let plain_chunk = mk
                        .open(&sealed_chunk)
                        .map_err(|e| std::io::Error::other(e.to_string()))?;
                    out.write_all(&plain_chunk)?;
                }
                Ok(())
            })();
            if ok.is_ok() && std::fs::rename(&tmp, &new_full).is_ok() {
                let _ = std::fs::remove_file(&full);
                converted += 1;
            } else {
                let _ = std::fs::remove_file(&tmp);
            }
            continue;
        }
        let tmp = new_full.with_extension("drive-sync-crypt.tmp");
        if std::fs::write(&tmp, &plain).is_ok() && std::fs::rename(&tmp, &new_full).is_ok() {
            if new_full != full {
                let _ = std::fs::remove_file(&full);
            }
            converted += 1;
        }
    }
    prune_empty_dirs(repo_path);
    converted
}

/// Removes empty directories left behind by the name migrations (bottom-up).
/// `.git` and `.chunks` stay untouched.
fn prune_empty_dirs(repo_path: &Path) {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![repo_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            if name == ".git" || (dir == repo_path && name == crate::chunk::CHUNKS_DIR) {
                continue;
            }
            if p.is_dir() {
                dirs.push(p.clone());
                stack.push(p);
            }
        }
    }
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(&d); // only removes if empty
    }
}

/// Removes from `.chunks/` the files that no manifest in the working tree
/// references. Cheap when there are no chunks (early return).
fn gc_orphan_chunks(repo_path: &Path, key: Option<&crate::crypt::Crypto>) {
    let chunks_root = repo_path.join(crate::chunk::CHUNKS_DIR);
    let existing: Vec<std::path::PathBuf> = walk_dir_files(&chunks_root);
    if existing.is_empty() {
        return;
    }
    let mut referenced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (full, _) in walk_worktree_files(repo_path) {
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        if !meta.is_file() || meta.len() > crate::chunk::MANIFEST_MAX_BYTES {
            continue;
        }
        let Ok(mut bytes) = std::fs::read(&full) else { continue };
        if crate::crypt::Crypto::is_encrypted_bytes(&bytes) {
            let Some(k) = key else {
                // Sealed manifests without a key: impossible to know the
                // references — NEVER collect blindly (it would remove live
                // chunks).
                eprintln!(
                    "[{}] chunk: gc skipped — encrypted worktree without an available key",
                    log_ts()
                );
                return;
            };
            match k.open(&bytes) {
                Ok(plain) => bytes = plain,
                Err(_) => continue,
            }
        }
        if crate::chunk::Manifest::is_manifest_bytes(&bytes) {
            if let Ok(m) = crate::chunk::Manifest::parse(&bytes) {
                referenced.extend(m.hashes);
            }
        }
    }
    for chunk_file in existing {
        let name = chunk_file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !referenced.contains(&name) {
            match std::fs::remove_file(&chunk_file) {
                Ok(_) => println!("[{}] chunk: gc removed orphan {}", log_ts(), name),
                Err(e) => eprintln!("[{}] chunk: gc failed on {}: {}", log_ts(), name, e),
            }
            // Empty shard dir goes with it (best-effort).
            if let Some(parent) = chunk_file.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

/// Working tree files as (full path, relative path), skipping `.git` at any
/// level and `.chunks` at the root. Does not follow symlinks.
fn walk_worktree_files(repo_path: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![repo_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let full = entry.path();
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            if dir == repo_path && name == crate::chunk::CHUNKS_DIR {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
            if meta.is_dir() {
                stack.push(full);
            } else if meta.is_file() || meta.file_type().is_symlink() {
                // Symlinks INCLUDED: the encryption migration needs to visit
                // them (name and target). Consumers that only handle regular
                // files check is_file() on their own.
                let rel = full
                    .strip_prefix(repo_path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push((full, rel));
            }
        }
    }
    out
}

/// Files (recursive) under a directory. Empty if the directory does not exist.
fn walk_dir_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
            if meta.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// `true` if the file starts with the manifest magic line (reads a few
/// bytes, without loading the whole file).
fn file_starts_with_manifest_magic(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 64];
    let Ok(n) = f.read(&mut buf) else { return false };
    crate::chunk::Manifest::is_manifest_bytes(&buf[..n])
}

/// Paths that macOS generates as noise and that must never leak into the
/// working tree/commit. Detects any path component matching a known pattern.
/// Paths that must never appear in the working tree — junk created and
/// deleted by macOS itself (AppleDouble, Finder/Spotlight/Trash metadata).
/// We filter ALL ops: we don't want even a trace of them in a commit.
pub(crate) fn is_fully_ephemeral_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    for component in path.split('/') {
        if component.starts_with("._") {
            return true;
        }
        if matches!(
            component,
            ".DS_Store"
                | ".Spotlight-V100"
                | ".Trashes"
                | ".TemporaryItems"
                | ".fseventsd"
                | ".DocumentRevisions-V100"
                | ".apdisk"
                | ".VolumeIcon.icns"
        ) {
            return true;
        }
    }
    false
}

/// TextEdit/NSDocument safe-save pattern: `<name>.sb-<hex>-<rand>`.
/// Temp dirs and hardlinked backups. We use this pattern only to block
/// **creation** (Write/Mkdir). Delete/Rmdir/Rename pass through, so the user
/// can clean up `.sb-*` residue already committed to the repo.
fn is_textedit_scratch_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    path.split('/').any(|c| c.contains(".sb-"))
}

fn op_touches_ephemeral(op: &SyncOp) -> bool {
    match op {
        SyncOp::Write { path, .. } | SyncOp::Mkdir { path } | SyncOp::Symlink { path, .. } => {
            is_fully_ephemeral_path(path) || is_textedit_scratch_path(path)
        }
        SyncOp::Delete { path } | SyncOp::Rmdir { path } => is_fully_ephemeral_path(path),
        SyncOp::Rename { from, to } => {
            is_fully_ephemeral_path(from)
                || is_fully_ephemeral_path(to)
                || is_textedit_scratch_path(to)
        }
    }
}

/// `true` if `a` and `b` are the same path or one is an ancestor of the
/// other (component boundary at '/': "a" contains "a/b", but not "ab").
fn paths_related(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_contains_b = b
        .strip_prefix(a)
        .map(|rest| rest.starts_with('/'))
        .unwrap_or(false);
    let b_contains_a = a
        .strip_prefix(b)
        .map(|rest| rest.starts_with('/'))
        .unwrap_or(false);
    a_contains_b || b_contains_a
}

/// Paths an op touches (rename touches both source and destination).
fn op_paths(op: &SyncOp) -> [Option<&str>; 2] {
    match op {
        SyncOp::Rename { from, to } => [Some(from), Some(to)],
        other => [Some(other.primary_path()), None],
    }
}

/// Appends `op` to the queue preserving arrival order. The only coalescing
/// allowed: a `Write` replaces in-place the most recent `Write` for the SAME
/// path, as long as no later op touches a related path (equal or
/// ancestor/descendant) — rename, delete, mkdir and rmdir are barriers.
/// Correctness never depends on the coalescing; it only avoids redundant
/// content during write storms (repeated flushes of the same file within the
/// debounce window).
pub(crate) fn push_coalescing(queue: &mut Vec<SyncOp>, op: SyncOp) {
    if let SyncOp::Write { path, content } = op {
        for existing in queue.iter_mut().rev() {
            if let SyncOp::Write { path: p, content: c } = existing {
                if *p == path {
                    // A replaced spooled snapshot will never reach the drain —
                    // removing the file here avoids buildup in the spool dir.
                    if let WriteContent::Spooled(old) = &*c {
                        let _ = std::fs::remove_file(old);
                    }
                    *c = content;
                    return;
                }
            }
            let blocking = op_paths(existing)
                .into_iter()
                .flatten()
                .any(|p| paths_related(p, &path));
            if blocking {
                break;
            }
        }
        queue.push(SyncOp::Write { path, content });
    } else {
        queue.push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> SyncManager {
        SyncManager::new("/tmp/ignored", 60)
    }

    /// Wraps bytes in WriteContent::Mem for the queue tests.
    fn wc(bytes: &[u8]) -> WriteContent {
        WriteContent::Mem(Arc::new(bytes.to_vec()))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_appends_to_buffer() {
        let m = mgr();
        m.queue_write("a.txt", b"hello".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { path, content } => {
                assert_eq!(path, "a.txt");
                assert_eq!(content.to_vec(), b"hello");
            }
            other => panic!("expected Write, got {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_twice_same_path_coalesces() {
        let m = mgr();
        m.queue_write("a.txt", b"first".to_vec());
        m.queue_write("a.txt", b"second".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { content, .. } => assert_eq!(content.to_vec(), b"second"),
            other => panic!("expected Write, got {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_mkdir_then_rmdir_same_path_keeps_causal_order() {
        // Causal queue semantics (Phase 0): both ops are kept in arrival
        // order — the drain applies mkdir and then rmdir (identical final
        // state, without relying on a "last one wins" heuristic).
        let m = mgr();
        m.queue_mkdir("foo");
        m.queue_rmdir("foo");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 2);
        assert!(matches!(snap[0], SyncOp::Mkdir { .. }));
        assert!(matches!(snap[1], SyncOp::Rmdir { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_rename_then_write_to_destination_keeps_both_in_order() {
        // Before (HashMap keyed by destination), the Write ERASED the Rename
        // from the queue. In the causal queue both survive, in arrival order.
        let m = mgr();
        m.queue_rename("a.txt", "b.txt");
        m.queue_write("b.txt", b"x".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 2);
        assert!(matches!(snap[0], SyncOp::Rename { .. }));
        assert!(matches!(snap[1], SyncOp::Write { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_delete_creates_delete_op() {
        let m = mgr();
        m.queue_delete("x.txt");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Delete { path } => assert_eq!(path, "x.txt"),
            other => panic!("expected Delete, got {:?}", other),
        }
    }

    #[test]
    fn is_ephemeral_path_flags_apple_double_and_system_dirs() {
        assert!(is_fully_ephemeral_path("._hi.txt"));
        assert!(is_fully_ephemeral_path("foo/._hi.txt"));
        assert!(is_fully_ephemeral_path(".DS_Store"));
        assert!(is_fully_ephemeral_path("docs/.DS_Store"));
        assert!(is_fully_ephemeral_path(".Spotlight-V100"));
        assert!(is_fully_ephemeral_path(".Trashes/whatever"));
        assert!(!is_fully_ephemeral_path("hi.txt"));
        assert!(!is_fully_ephemeral_path("foo/hi.txt"));
        // .sb-* is not covered by `is_ephemeral_path` (full filter) — only by
        // `is_textedit_scratch_path`, which is partial.
        assert!(!is_fully_ephemeral_path("hi.txt.sb-36d9af71-iFhNoR"));
        assert!(!is_fully_ephemeral_path(""));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn textedit_scratch_paths_block_creation_but_allow_delete() {
        let m = mgr();
        // Creation: filtered out
        m.queue_write("hi.txt.sb-36d9af71-backup", b"x".to_vec());
        m.queue_mkdir("hi.txt.sb-36d9af71-tempdir");
        // Delete/rmdir: passes through (the user can clean up legacy `.sb-*`)
        m.queue_delete("hi.txt.sb-36d9af71-old-backup");
        m.queue_rmdir("hi.txt.sb-36d9af71-old-tempdir");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 2, "expected only Delete+Rmdir, got {:?}", snap);
        assert!(snap.iter().any(|op| matches!(op, SyncOp::Delete { .. })));
        assert!(snap.iter().any(|op| matches!(op, SyncOp::Rmdir { .. })));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_drops_ephemeral_path() {
        let m = mgr();
        m.queue_write("._hi.txt", b"junk".to_vec());
        m.queue_write(".DS_Store", b"junk".to_vec());
        m.queue_write("hi.txt", b"real".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { path, .. } => assert_eq!(path, "hi.txt"),
            other => panic!("expected Write for hi.txt, got {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_rename_drops_when_either_side_is_ephemeral() {
        let m = mgr();
        m.queue_rename("hi.txt", "._hi.txt");
        m.queue_rename("._hi.txt", "hi.txt");
        assert_eq!(m.buffer_snapshot().len(), 0);
    }

    #[test]
    fn push_coalescing_write_over_write_same_path_replaces_in_place() {
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "a.txt".into(), content: wc(b"v1") });
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: wc(b"other") });
        push_coalescing(&mut q, SyncOp::Write { path: "a.txt".into(), content: wc(b"v2") });
        assert_eq!(q.len(), 2);
        assert!(matches!(&q[0], SyncOp::Write { path, content }
            if path == "a.txt" && content.to_vec() == b"v2"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_symlink_appends_and_respects_ephemeral_filter() {
        let m = mgr();
        m.queue_symlink("alias.txt", "real.txt");
        m.queue_symlink(".DS_Store", "x");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1, "ephemeral path must be filtered out: {:?}", snap);
        match &snap[0] {
            SyncOp::Symlink { path, target } => {
                assert_eq!(path, "alias.txt");
                assert_eq!(target, "real.txt");
            }
            other => panic!("expected Symlink, got {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_content_shares_arc_without_copy() {
        // Requirement 16 of Phase 2.5: the buffer and SyncOp::Write share the
        // SAME content (Arc) — RAM peak ~1x, not ~3x the size.
        let m = mgr();
        let content = std::sync::Arc::new(b"large content".to_vec());
        m.queue_write_content("a.txt", WriteContent::Mem(content.clone()));
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { content: WriteContent::Mem(arc), .. } => {
                assert!(
                    std::sync::Arc::ptr_eq(arc, &content),
                    "content must be shared via Arc, not cloned"
                );
            }
            other => panic!("expected shared Write, got {:?}", other),
        }
    }

    #[test]
    fn push_coalescing_symlink_is_barrier_for_write() {
        // A post-symlink Write must not coalesce with a pre-symlink Write on
        // the same path: `rm f && ln -s target f && echo x > f` are three
        // causal states.
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "f.txt".into(), content: wc(b"pre") });
        push_coalescing(&mut q, SyncOp::Symlink { path: "f.txt".into(), target: "target".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "f.txt".into(), content: wc(b"post") });
        assert_eq!(q.len(), 3, "Symlink must be a barrier: {:?}", q);
        assert!(matches!(&q[2], SyncOp::Write { content, .. } if content.to_vec() == b"post"));
    }

    #[test]
    fn push_coalescing_rename_is_barrier_for_write() {
        // Write(b) must not cross Rename{a->b}: the pre- and post-rename
        // content are different states of the path.
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: wc(b"pre") });
        push_coalescing(&mut q, SyncOp::Rename { from: "a.txt".into(), to: "b.txt".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: wc(b"post") });
        assert_eq!(q.len(), 3);
        assert!(matches!(&q[2], SyncOp::Write { content, .. } if content.to_vec() == b"post"));
    }

    #[test]
    fn push_coalescing_dir_rename_is_barrier_for_nested_write() {
        // A directory rename is a barrier for writes INSIDE it (prefix).
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "d/f.txt".into(), content: wc(b"v1") });
        push_coalescing(&mut q, SyncOp::Rename { from: "c".into(), to: "d".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "d/f.txt".into(), content: wc(b"v2") });
        assert_eq!(q.len(), 3, "post-rename write must not coalesce with the pre-rename one");
    }

    #[test]
    fn paths_related_respects_component_boundary() {
        assert!(paths_related("a", "a"));
        assert!(paths_related("a", "a/b"));
        assert!(paths_related("a/b/c", "a/b"));
        assert!(!paths_related("a", "ab"));
        assert!(!paths_related("ab", "a"));
        assert!(!paths_related("a/b", "a/bc"));
    }

    #[test]
    fn build_commit_message_three_paths() {
        let ops_owned = [
            SyncOp::Write { path: "a.txt".into(), content: wc(b"") },
            SyncOp::Write { path: "b.txt".into(), content: wc(b"") },
            SyncOp::Write { path: "c.txt".into(), content: wc(b"") },
        ];
        let ops: Vec<&SyncOp> = ops_owned.iter().collect();
        let m = build_commit_message(&ops);
        assert!(m.contains("3 changes"), "{}", m);
        assert!(m.contains("a.txt"));
    }

    #[test]
    fn build_commit_message_more_than_three_truncates() {
        let ops_owned: Vec<SyncOp> = (0..5)
            .map(|i| SyncOp::Write {
                path: format!("f{}.txt", i),
                content: wc(b""),
            })
            .collect();
        let ops: Vec<&SyncOp> = ops_owned.iter().collect();
        let m = build_commit_message(&ops);
        assert!(m.contains("5 changes"), "{}", m);
        assert!(m.contains("+2 more"), "{}", m);
    }
}
