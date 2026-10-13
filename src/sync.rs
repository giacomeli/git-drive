use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::{sleep, Instant};
use tokio::task;

use crate::git;
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
    /// Maximum content bytes per drain batch: the queue is split into
    /// size-bounded batches, each one its own commit, published one by one
    /// (bounded push packs). 0 = whole drain in a single commit.
    commit_batch_bytes: Arc<AtomicU64>,
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
    /// Write-ahead journal of the queue (crash durability); `None` = off.
    journal: Option<Arc<crate::journal::Journal>>,
    /// Seq of the next journal record. Assigned under this lock together with
    /// the push into `buffer`, so a drain's boundary (`next_seq - 1` at take
    /// time) covers exactly the ops it took.
    next_seq: u64,
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

    #[allow(clippy::too_many_arguments)]
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
                journal: None,
                next_seq: 1,
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
            commit_batch_bytes: Arc::new(AtomicU64::new(256 * 1024 * 1024)),
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

    /// Configures the drain batch limit in bytes (0 = single batch).
    /// Post-construction setter, like `set_chunking`.
    pub fn set_batching(&self, batch_bytes: u64) {
        self.commit_batch_bytes.store(batch_bytes, Ordering::SeqCst);
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
            commit_batch_bytes: self.commit_batch_bytes.clone(),
            crypto: self.crypto.clone(),
            migration_crypto: self.migration_crypto.clone(),
        }
    }

    /// Drains the queue NOW, synchronously (apply + commit + push, under the
    /// `repo_lock`). Used during clean shutdown: after the unmount, it
    /// guarantees no queued op is lost when the process dies.
    pub fn drain_now(&self) {
        let (changes, boundary) = take_queue(&self.inner);
        if changes.is_empty() {
            return;
        }
        log::info!("drain_now: draining {} pending op(s)", changes.len());
        self.drain_ctx().run(changes, boundary);
    }

    /// Enables the write-ahead journal in `dir` and replays the records a
    /// previous session left behind (it crashed before committing them) into
    /// the queue, in their original order. Returns how many were replayed.
    pub fn enable_journal(&self, dir: &Path) -> std::io::Result<usize> {
        let (journal, records) = crate::journal::Journal::open(dir)?;
        let replayed = records.len();
        let max_seq = records.last().map(|r| r.0).unwrap_or(0);
        {
            let mut inner = self.inner.lock();
            for (_, op) in records {
                push_coalescing(&mut inner.buffer, op);
            }
            inner.next_seq = inner.next_seq.max(max_seq + 1);
            inner.journal = Some(Arc::new(journal));
            if replayed > 0 {
                inner.last_op = Some(Instant::now());
            }
        }
        if replayed > 0 {
            log::warn!(
                "journal: {} op(s) from an interrupted session replayed into the queue",
                replayed
            );
            self.notify.notify_one();
        }
        Ok(replayed)
    }

    /// Boot recovery, run BEFORE the mount: drains replayed journal ops right
    /// away (the drive then boots on a HEAD that already contains them — no
    /// window where the user sees and edits the stale version), and commits a
    /// working tree left dirty by a crash between apply and commit.
    pub fn recover_at_boot(&self) {
        if self.has_pending_ops() {
            self.drain_now();
            return;
        }
        let dirty = git::open_repo(self.repo_path.to_str().unwrap_or(""))
            .and_then(|r| git::is_working_tree_clean(&r))
            .map(|clean| !clean)
            .unwrap_or(false);
        if dirty {
            log::warn!("boot: working tree has uncommitted changes — committing them");
            self.drain_ctx().run(Vec::new(), None);
        }
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
        let crypto = self.crypto.clone();

        task::spawn(async move {
            // The first pull runs right after boot (not one full interval
            // later): a machine waking up or relaunching catches up with the
            // remote — and publishes commits a previous session left unpushed
            // — within seconds.
            let mut delay = interval_secs.min(3);
            loop {
                sleep(Duration::from_secs(delay)).await;
                delay = interval_secs;

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
                        log::error!("pull: error opening repo: {}", e);
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
                        log::warn!(
                            "pull: working tree has uncommitted changes — skipping"
                        );
                        status.record_pull("skipped: dirty working tree");
                        notifier.notify(NotifyEvent::PullBlocked);
                        continue;
                    }
                    Err(e) => {
                        log::error!("pull: error checking working tree: {}", e);
                        status.record_pull("error checking working tree");
                        continue;
                    }
                }

                if let Err(e) =
                    git::fetch_remote(&repo, &branch, remote, remote_token.as_deref())
                {
                    log::error!("pull: fetch error: {}", e);
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
                        log::info!("pull: fast-forward applied");
                        status.record_pull("fast-forward");
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Ok(git::MergeOutcome::Merged) => {
                        log::info!("pull: 3-way merge, no conflicts");
                        status.record_pull("3-way merge");
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Ok(git::MergeOutcome::Conflicted { resolved, suffix }) => {
                        log::warn!(
                            "pull: resolved {} conflict(s) — local versions at *.{}: {:?}",
                            resolved.len(),
                            suffix,
                            resolved
                        );
                        status.record_pull(&format!("merge with {} conflict(s)", resolved.len()));
                        let c = crypto.lock().clone();
                        let copies =
                            conflict_copies_for_display(&repo_path, c.as_deref(), &resolved, &suffix);
                        status.add_conflicts(copies.clone());
                        notifier.notify(NotifyEvent::Conflicts { copies });
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Err(e) => {
                        log::error!("pull: merge error: {}", e);
                        status.record_pull("merge error");
                        continue;
                    }
                }

                // Local commits the remote does not have yet — a push that
                // failed while offline, a previous session that died before
                // publishing, or the merge commit just created above — are
                // published HERE. Leaving them to the next drain meant a
                // machine that went quiet after an offline write never synced.
                if local_ahead_of_remote(&repo, &branch, remote) {
                    drop(repo);
                    let ok = publish_progressive(
                        &repo_path,
                        &branch,
                        remote,
                        &remote_token,
                        &status,
                    )
                    .ok();
                    if ok {
                        notifier.reset(notify::KEY_PUSH_FAILED);
                    } else {
                        notifier.notify(NotifyEvent::PushFailed);
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
        // Journal first (content bytes written OUTSIDE the queue lock), then
        // seq + push under the lock — see `journal.rs`.
        let journal = self.inner.lock().journal.clone();
        let prepared = journal.as_ref().and_then(|j| match j.prepare(&op) {
            Ok(p) => Some(p),
            Err(e) => {
                log::error!("journal: could not persist op on {}: {}", op.primary_path(), e);
                None
            }
        });
        {
            let mut inner = self.inner.lock();
            if let (Some(j), Some(p)) = (journal.as_ref(), prepared) {
                let seq = inner.next_seq;
                inner.next_seq += 1;
                if let Err(e) = j.commit(p, seq) {
                    log::error!("journal: could not commit record {}: {}", seq, e);
                }
            }
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

                    let should_drain = {
                        let inner = ctx.inner.lock();
                        inner
                            .last_op
                            .map(|l| l.elapsed() >= Duration::from_secs(debounce_secs))
                            .unwrap_or(false)
                            && !inner.buffer.is_empty()
                    };

                    if should_drain {
                        let (changes, boundary) = take_queue(&ctx.inner);
                        if !changes.is_empty() {
                            log::info!("drain: syncing changes");
                            ctx.run(changes, boundary);
                        }
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
    commit_batch_bytes: Arc<AtomicU64>,
    crypto: Arc<Mutex<Option<Arc<crate::crypt::Crypto>>>>,
    migration_crypto: Arc<Mutex<Option<Arc<crate::crypt::Crypto>>>>,
}

/// Takes the whole queue plus its journal boundary (the last seq the taken
/// ops cover; `None` without a journal).
fn take_queue(inner: &Mutex<Inner>) -> (Vec<SyncOp>, Option<u64>) {
    let mut inner = inner.lock();
    let changes = std::mem::take(&mut inner.buffer);
    let boundary = inner.journal.as_ref().map(|_| inner.next_seq - 1);
    (changes, boundary)
}

impl DrainCtx {
    fn run(&self, changes: Vec<SyncOp>, journal_boundary: Option<u64>) {
        // Serialize the whole cycle (apply + commit + push + reconcile) with
        // the pull watcher (RF4 of Phase 0). No .await in the guard's scope.
        let _repo_guard = self.repo_lock.lock();
        let crypto = self.crypto.lock().clone();
        let migration_crypto = self.migration_crypto.lock().clone();
        let chunk_threshold = self.chunk_threshold_bytes.load(Ordering::SeqCst);
        let chunk_size = self.chunk_size_bytes.load(Ordering::SeqCst).max(1);
        let batch_limit = self.commit_batch_bytes.load(Ordering::SeqCst);

        // Phase A (local durability first): the causal queue is split into
        // size-bounded batches, each applied and committed on its own — a mass
        // copy becomes N bounded commits instead of one giant one (the 24 GB
        // single-pack push is what GitHub aborted in the 2026-07-10 incident).
        // Consuming batch by batch also drops each batch's content `Arc`s right
        // after its commit, so the RAM peak is ~one batch, not the whole queue.
        let total_ops = changes.len();
        let batches = split_batches(changes, batch_limit);
        let n_batches = batches.len();
        if n_batches > 1 {
            log::info!(
                "drain: {} op(s) split into {} batch(es) of up to {} MB",
                total_ops,
                n_batches,
                batch_limit / (1024 * 1024)
            );
        }
        let mut last_commit: Option<git2::Oid> = None;
        let mut commits_created = 0usize;
        for (i, batch) in batches.into_iter().enumerate() {
            // Apply in arrival order — the queue is already causal. With
            // encryption, the cleartext->encrypted translation of paths and
            // content happens HERE (the FUSE layer and the queue work 100% in
            // cleartext).
            let ops: Vec<&SyncOp> = batch.iter().collect();
            for op in &ops {
                apply_op_to_disk(&self.repo_path, op, crypto.as_deref(), chunk_threshold);
            }
            // Chunking (Phase 2.6): the sweep converts ANY raw file above the
            // threshold — with encryption, each chunk is sealed and the hash
            // is over the ciphertext. Invariant: no raw blob above the
            // threshold is staged (per batch: no batch commit may contain an
            // oversized raw blob). Then the state migrations (Phase 2.7)
            // conform the worktree; the GC removes orphan chunks last.
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
                    log::warn!(
                        "crypto: {} item(s) migrated to encrypted. WARNING: the PREVIOUS Git history remains in cleartext on the remote — for a public repo, prefer starting from a fresh repo",
                        migrated
                    );
                }
                converted += migrated;
            } else if let Some(mk) = migration_crypto.as_deref() {
                let migrated = migrate_worktree_to_clear(&self.repo_path, mk);
                if migrated > 0 {
                    log::info!(
                        "crypto: {} item(s) decrypted back to cleartext",
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
                crypto.is_some(),
            );
            if let Some(oid) = commit_oid {
                commits_created += 1;
                last_commit = Some(oid);
                if n_batches > 1 {
                    log::info!("drain: batch {}/{} committed", i + 1, n_batches);
                }
                self.commit_generation.fetch_add(1, Ordering::SeqCst);
                // The sweep's conversion creates new paths in HEAD that the
                // FUSE index may not know about (file arrived from outside the
                // mount) — without the refresh, the file would stay invisible
                // in the drive until a real pull (zip migration incident,
                // 2026-07-03). Flag AFTER the commit: a rebuild before it
                // would read the old HEAD and consume the flag for nothing.
                if converted > 0 {
                    self.refresh_flag.store(true, Ordering::SeqCst);
                }
            }
            // Heartbeat per batch: keeps updated_at fresh during long drains
            // (the tray marked the daemon stopped after 15 quiet minutes) and
            // publishes how many commits still await push.
            let pending = self.inner.lock().buffer.len();
            self.status.record_drain(
                ops.len(),
                commit_oid.map(|o| o.to_string()),
                None,
                pending,
            );
            if self.remote_name.is_some() {
                self.status.set_unpushed(commits_created);
            }
            drop(ops);
            drop(batch); // free this batch's content before the next one
        }

        // Every taken op is now applied and committed (or applied without a
        // commit in identity-less mode — the working tree holds it): its
        // journal records are no longer needed. Still under the repo lock, so
        // no pull can land between the commit and this cleanup.
        if let Some(boundary) = journal_boundary {
            let journal = self.inner.lock().journal.clone();
            if let Some(j) = journal {
                j.truncate_through(boundary);
            }
        }

        // Phase B (publish): push commit by commit — every pack stays around
        // one batch. A mid-catch-up failure leaves the remote on an
        // intermediate commit; the next cycle resumes from there.
        let mut pushed: Option<bool> = None;
        if last_commit.is_some() {
            if let Some(ref remote) = self.remote_name {
                let ok = publish_progressive(
                    &self.repo_path,
                    &self.branch,
                    remote,
                    &self.remote_token,
                    &self.status,
                )
                .ok();
                if ok {
                    pushed = Some(true);
                    self.notifier.reset(notify::KEY_PUSH_FAILED);
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
                        crypto.as_deref(),
                    );
                    pushed = Some(recovered);
                    if recovered {
                        self.refresh_flag.store(true, Ordering::SeqCst);
                        self.notifier.reset(notify::KEY_PUSH_FAILED);
                    } else {
                        // Push + reconcile failed: local commits left unpublished.
                        self.notifier.notify(NotifyEvent::PushFailed);
                    }
                }
            }
        }
        let pending = self.inner.lock().buffer.len();
        self.status.record_drain(
            total_ops,
            last_commit.map(|o| o.to_string()),
            pushed,
            pending,
        );
    }
}

/// Content bytes an op contributes to a batch (only writes carry content;
/// metadata ops — delete/mkdir/rmdir/rename/symlink — are free).
fn op_content_len(op: &SyncOp) -> u64 {
    match op {
        SyncOp::Write { content, .. } => content.len(),
        _ => 0,
    }
}

/// Splits the causal queue into consecutive batches whose accumulated Write
/// content stays within `limit` bytes. Pure prefix cuts — arrival order is
/// preserved inside and across batches, so the Phase 0 causality invariant
/// holds. A single write larger than the limit forms a batch of its own (a
/// file cannot be split across commits — its chunks land together). `limit`
/// 0 disables batching (single batch).
pub(crate) fn split_batches(changes: Vec<SyncOp>, limit: u64) -> Vec<Vec<SyncOp>> {
    if limit == 0 || changes.is_empty() {
        return vec![changes];
    }
    let mut batches = Vec::new();
    let mut current: Vec<SyncOp> = Vec::new();
    let mut acc = 0u64;
    for op in changes {
        let len = op_content_len(&op);
        if !current.is_empty() && acc.saturating_add(len) > limit {
            batches.push(std::mem::take(&mut current));
            acc = 0;
        }
        acc = acc.saturating_add(len);
        current.push(op);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

/// True when the local branch has commits that `refs/remotes/<remote>/<branch>`
/// lacks AND the remote tip is an ancestor (a plain catch-up push is possible).
/// A missing remote-tracking ref (empty remote) counts as ahead.
fn local_ahead_of_remote(repo: &git2::Repository, branch: &str, remote: &str) -> bool {
    let Ok(local) = repo.refname_to_id(&format!("refs/heads/{}", branch)) else {
        return false;
    };
    match repo.refname_to_id(&format!("refs/remotes/{}/{}", remote, branch)) {
        Ok(tip) => tip != local && repo.graph_descendant_of(local, tip).unwrap_or(false),
        Err(_) => true,
    }
}

/// Publishes every commit missing on the remote, one push per commit (bounded
/// packs). Progress heartbeats go to the status (tray stays alive during long
/// uploads). Returns `true` when the remote reached the local tip.
/// Outcome of `publish_progressive`.
#[derive(Debug, PartialEq, Eq)]
enum Publish {
    /// The remote reached the local tip.
    Done,
    /// Another client pushed first (fast-forward impossible): reconcile.
    RemoteMoved,
    /// Network/auth/server failure.
    Failed,
}

impl Publish {
    fn ok(&self) -> bool {
        *self == Publish::Done
    }
}

fn publish_progressive(
    repo_path: &Path,
    branch: &str,
    remote: &str,
    token: &Option<String>,
    status: &StatusState,
) -> Publish {
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            log::error!("push: error opening repo: {}", e);
            return Publish::Failed;
        }
    };
    // Throttled transfer heartbeat: a single pack can take minutes on a slow
    // uplink; without touches the tray goes gray at 180 s of silence.
    let last_touch = std::cell::Cell::new(std::time::Instant::now());
    let result = git::push_progressive(&repo, branch, remote, token.as_deref(), &|ev| {
        match ev {
            git::PushProgress::Transfer => {
                if last_touch.get().elapsed() >= Duration::from_secs(30) {
                    last_touch.set(std::time::Instant::now());
                    status.touch();
                }
            }
            git::PushProgress::CommitPushed { done, total } => {
                log::info!("push: {} catch-up {}/{} ok", remote, done, total);
                status.set_unpushed(total - done);
            }
        }
    });
    match result {
        Ok(0) => {
            log::info!("push: {} up to date", remote);
            status.set_unpushed(0);
            Publish::Done
        }
        Ok(n) => {
            log::info!("push: {} ok ({} commit(s) published)", remote, n);
            status.set_unpushed(0);
            Publish::Done
        }
        Err(e) => {
            if is_remote_moved(e.message()) {
                // Another client pushed first: the expected concurrent case,
                // resolved by the reconcile (fetch + merge + push) right after.
                log::warn!("push: {} moved (another client pushed) — reconciling", remote);
                Publish::RemoteMoved
            } else {
                log::error!("push: error on {}: {}", remote, e);
                Publish::Failed
            }
        }
    }
}

/// True when a push failed only because the remote branch advanced (a
/// fast-forward is no longer possible) — not a network/auth/server error.
fn is_remote_moved(msg: &str) -> bool {
    msg.contains("not an ancestor")
        || msg.contains("non-fast-forward")
        || msg.contains("fetch first")
        || msg.contains("NotFastForward")
        || msg.contains("cannot push non-fastforwardable")
        // A concurrent push landing on the remote ref at the same instant.
        || msg.contains("cannot lock ref")
}

/// Translates a CLEARTEXT path from the queue into the on-disk path
/// (encrypted when encryption is active). Long components map to their bounded
/// `dslong_<hash>` name; the matching sidecar is materialized separately by
/// `write_sidecars` on the write path.
fn disk_rel_path(crypto: Option<&crate::crypt::Crypto>, clear: &str) -> String {
    match crypto {
        Some(c) => c.enc_path(clear),
        None => clear.to_string(),
    }
}

/// Materializes the long-name sidecars (`<disk>.name`) for every long component
/// along `clear`. Idempotent (deterministic content). No-op without crypto or
/// without any long component.
fn write_sidecars(repo_path: &Path, crypto: Option<&crate::crypt::Crypto>, clear: &str) {
    let Some(c) = crypto else { return };
    for sc in c.enc_path_full(clear).sidecars {
        let full = repo_path.join(&sc.path);
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let needs = std::fs::read(&full)
            .map(|b| b != sc.content.as_bytes())
            .unwrap_or(true);
        if needs {
            if let Err(e) = std::fs::write(&full, sc.content.as_bytes()) {
                log::error!("crypto: failed to write long-name sidecar {:?}: {}", full, e);
            }
        }
    }
}

/// Rebuilds an ON-DISK path to the canonical encrypted form, component by
/// component: hashed `dslong_` entries are preserved verbatim (their cleartext
/// lives in their sidecar — a whole-path dec+enc would re-encrypt the literal
/// hash and corrupt the entry), conforming encrypted components stay as they
/// are (deterministic re-encode), over-threshold pre-threshold components are
/// collapsed to `dslong_` + sidecar, and cleartext components are encrypted
/// (preserving a conflicted-copy suffix). Returns the new rel path plus the
/// sidecars to materialize as `(rel_path, content)`.
fn canonicalize_disk_path(
    c: &crate::crypt::Crypto,
    rel: &str,
) -> (String, Vec<(String, String)>) {
    let mut parts: Vec<String> = Vec::new();
    let mut sidecars: Vec<(String, String)> = Vec::new();
    for comp in rel.split('/') {
        if comp.is_empty() {
            continue;
        }
        if crate::crypt::is_long_name(comp) {
            parts.push(comp.to_string());
            continue;
        }
        let e = match c.dec_component(comp) {
            // Valid encrypted component: re-encode from its cleartext —
            // identity for conforming names, dslong for over-threshold ones.
            Some(clear) => c.enc_component(&clear),
            None => match c.dec_conflicted(comp) {
                // Conflicted copy a merge made of an ENCRYPTED name
                // (`<encrypted>.conflict-...`): re-encode the clear base and
                // keep the suffix. Treating the encrypted base as cleartext
                // encrypted it a second time — the copy then showed up on the
                // drive under the base32 name of the original (2026-10-13).
                Some((clear, suffix)) => {
                    let e = c.enc_component(&clear);
                    crate::crypt::EncName {
                        disk: format!("{}{}", e.disk, suffix),
                        sidecar: e.sidecar,
                    }
                }
                // Cleartext (or cleartext conflicted) component.
                None => c.enc_component_keep_conflict(comp),
            },
        };
        if let Some(content) = e.sidecar {
            let dir = parts.join("/");
            let name = crate::crypt::sidecar_name(&e.disk);
            let sc_rel = if dir.is_empty() {
                name
            } else {
                format!("{}/{}", dir, name)
            };
            sidecars.push((sc_rel, content));
        }
        parts.push(e.disk);
    }
    (parts.join("/"), sidecars)
}

/// Writes the sidecar files produced by `canonicalize_disk_path`. Idempotent.
fn materialize_sidecars(repo_path: &Path, sidecars: &[(String, String)]) {
    for (rel, content) in sidecars {
        let full = repo_path.join(rel);
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let needs = std::fs::read(&full)
            .map(|b| b != content.as_bytes())
            .unwrap_or(true);
        if needs {
            if let Err(e) = std::fs::write(&full, content.as_bytes()) {
                log::error!(
                    "crypto: failed to write long-name sidecar {:?}: {}",
                    full,
                    e
                );
            }
        }
    }
}

/// Removes the sidecar of `clear`'s LEAF component, if it is a long name.
/// Parent-directory sidecars are left in place (siblings still need them).
/// Used on unlink/rmdir/rename-source.
fn remove_leaf_sidecar(repo_path: &Path, crypto: Option<&crate::crypt::Crypto>, clear: &str) {
    let Some(c) = crypto else { return };
    let disk = c.enc_path(clear);
    let (dir, leaf) = match disk.rsplit_once('/') {
        Some((d, l)) => (Some(d), l),
        None => (None, disk.as_str()),
    };
    if !crate::crypt::is_long_name(leaf) {
        return;
    }
    let sidecar_rel = match dir {
        Some(d) => format!("{}/{}", d, crate::crypt::sidecar_name(leaf)),
        None => crate::crypt::sidecar_name(leaf),
    };
    let _ = std::fs::remove_file(repo_path.join(sidecar_rel));
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
                Ok(_) => {
                    write_sidecars(repo_path, crypto, path);
                    log::trace!("disk: write {}", path);
                }
                Err(e) => log::error!("disk: error saving {}: {}", path, e),
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
                Ok(_) => log::trace!("disk: unlink {}", path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // The path never reached the disk (e.g. `.sb-*` temp dirs
                    // that the filter blocks on creation but lets through on
                    // delete). Idempotent: already gone = successful removal.
                }
                Err(e) => log::error!("disk: error removing {}: {}", path, e),
            }
            remove_leaf_sidecar(repo_path, crypto, path);
        }
        SyncOp::Mkdir { path } => {
            let full = repo_path.join(disk_rel_path(crypto, path));
            if let Err(e) = std::fs::create_dir_all(&full) {
                log::error!("disk: error creating directory {}: {}", path, e);
                return;
            }
            write_sidecars(repo_path, crypto, path);
            let keep_rel = disk_rel_path(crypto, &format!("{}/.gitkeep", path));
            let keep = repo_path.join(keep_rel);
            if !keep.exists() {
                let keep_bytes: Vec<u8> = match crypto {
                    Some(c) => c.seal(b""),
                    None => Vec::new(),
                };
                if let Err(e) = std::fs::write(&keep, keep_bytes) {
                    log::error!(
                        "disk: error creating .gitkeep in {}: {}",
                        path,
                        e
                    );
                    return;
                }
            }
            log::trace!("disk: mkdir {}", path);
        }
        SyncOp::Rmdir { path } => {
            let full = repo_path.join(disk_rel_path(crypto, path));
            let keep = repo_path.join(disk_rel_path(crypto, &format!("{}/.gitkeep", path)));
            let _ = std::fs::remove_file(&keep);
            match std::fs::remove_dir(&full) {
                Ok(_) => log::trace!("disk: rmdir {}", path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Same rationale as Delete: a directory that never reached
                    // the disk (e.g. `.sb-*`) yields ENOENT on rmdir. Silence it.
                }
                Err(e) => log::error!(
                    "disk: error removing directory {}: {}",
                    path,
                    e
                ),
            }
            remove_leaf_sidecar(repo_path, crypto, path);
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
                Ok(_) => {
                    remove_leaf_sidecar(repo_path, crypto, from);
                    write_sidecars(repo_path, crypto, to);
                    log::trace!("disk: rename {} -> {}", from, to);
                }
                Err(e) => log::error!(
                    "disk: error renaming {} -> {}: {}",
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
                Ok(_) => {
                    write_sidecars(repo_path, crypto, path);
                    log::trace!("disk: symlink {} -> {}", path, target);
                }
                Err(e) => log::error!(
                    "disk: error creating symlink {} -> {}: {}",
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
    redact_paths: bool,
) -> Option<git2::Oid> {
    let (Some(name), Some(email)) = (user_name.as_deref(), user_email.as_deref()) else {
        log::warn!(
            "commit: user_name/user_email missing — skipping commit"
        );
        return None;
    };
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            log::error!("commit: error opening repo: {}", e);
            return None;
        }
    };
    let message = build_commit_message(ops, redact_paths);
    match git::commit_working_tree(&repo, branch, name, email, &message) {
        Ok(Some(oid)) => {
            log::info!("commit: created {}", oid);
            Some(oid)
        }
        Ok(None) => {
            log::info!("commit: clean tree, nothing to commit");
            None
        }
        Err(e) => {
            log::error!("commit: error: {}", e);
            None
        }
    }
}

/// The push failed (usually non-FF). Does fetch + pull_and_merge + push, up
/// to `RECONCILE_ROUNDS` times: with several clients writing at once, the
/// remote can move AGAIN between this merge and the retry (a third client's
/// push) — that is a race to re-run, not a failure to report. Returns `true`
/// if a retry succeeded (the working tree changed — the caller signals a
/// refresh to the FUSE layer).
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
    crypto: Option<&crate::crypt::Crypto>,
) -> bool {
    const RECONCILE_ROUNDS: usize = 3;
    let (Some(name), Some(email)) = (user_name.as_deref(), user_email.as_deref()) else {
        log::error!(
            "reconcile: push failed and no identity configured — cannot reconcile"
        );
        return false;
    };
    for round in 1..=RECONCILE_ROUNDS {
        let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
            Ok(r) => r,
            Err(e) => {
                log::error!("reconcile: error opening repo: {}", e);
                return false;
            }
        };

        if let Err(e) = git::fetch_remote(&repo, branch, remote, token.as_deref()) {
            log::error!("reconcile: fetch error: {}", e);
            return false;
        }

        let suffix = conflict_suffix();
        match git::pull_and_merge(&repo, branch, remote, name, email, &suffix) {
            Ok(git::MergeOutcome::NoOp) if round == 1 => {
                log::error!(
                    "reconcile: no divergence — the original error may be network/auth"
                );
                return false;
            }
            Ok(git::MergeOutcome::NoOp) => {}
            Ok(git::MergeOutcome::Conflicted { ref resolved, ref suffix }) => {
                log::warn!(
                    "reconcile: {} conflict(s) — local versions at *.{}: {:?}",
                    resolved.len(),
                    suffix,
                    resolved
                );
                let copies = conflict_copies_for_display(repo_path, crypto, resolved, suffix);
                status.add_conflicts(copies.clone());
                notifier.notify(NotifyEvent::Conflicts { copies });
            }
            Ok(o) => {
                log::info!("reconcile: applied ({:?})", o);
            }
            Err(e) => {
                log::error!("reconcile: pull_and_merge error: {}", e);
                return false;
            }
        }

        // The retry is progressive too: after a merge, the missing history can
        // span many batch commits — pushing the tip here would rebuild the very
        // giant pack that failed in the first place.
        drop(repo);
        match publish_progressive(repo_path, branch, remote, token, status) {
            Publish::Done => {
                log::info!("push: retry on {} ok", remote);
                return true;
            }
            Publish::RemoteMoved if round < RECONCILE_ROUNDS => {
                log::info!("reconcile: remote moved again — round {}", round + 1);
            }
            _ => {
                log::error!("push: retry failed again");
                return false;
            }
        }
    }
    false
}

/// Conflicted-copy paths as the user sees them on the drive. The merge works
/// on ON-DISK paths, which in encrypted mode are base32/`dslong_` names —
/// meaningless in the status, the tray and the notification.
fn conflict_copies_for_display(
    repo_path: &Path,
    crypto: Option<&crate::crypt::Crypto>,
    resolved: &[String],
    suffix: &str,
) -> Vec<String> {
    resolved
        .iter()
        .map(|p| {
            let disk = format!("{}.{}", p, suffix);
            match crypto {
                Some(c) => clear_path_from_disk(repo_path, c, &disk),
                None => disk,
            }
        })
        .collect()
}

/// Decrypts an on-disk path component by component, resolving hashed
/// long names through their sidecar files in the working tree (exact
/// `<leaf>.name`, or the conflict base `dslong_<h>.name`). Unresolvable
/// components stay literal.
fn clear_path_from_disk(repo_path: &Path, c: &crate::crypt::Crypto, disk: &str) -> String {
    let mut dir = PathBuf::new();
    let mut clear: Vec<String> = Vec::new();
    for comp in disk.split('/') {
        let name = if crate::crypt::is_long_name(comp) {
            let exact = repo_path.join(&dir).join(crate::crypt::sidecar_name(comp));
            let content = std::fs::read_to_string(&exact).ok().or_else(|| {
                crate::crypt::long_name_base(comp).and_then(|base| {
                    std::fs::read_to_string(
                        repo_path.join(&dir).join(crate::crypt::sidecar_name(base)),
                    )
                    .ok()
                })
            });
            content
                .and_then(|ct| c.dec_long(comp, ct.trim()))
                .unwrap_or_else(|| comp.to_string())
        } else {
            c.dec_path(comp)
        };
        clear.push(name);
        dir.push(comp);
    }
    clear.join("/")
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

fn build_commit_message(ops: &[&SyncOp], redact_paths: bool) -> String {
    let n = ops.len();
    if n == 0 {
        // Boot recovery of a dirty working tree (no queued op behind it).
        return "git-drive: recover uncommitted changes".to_string();
    }
    // Encrypted repo: the queue paths are CLEARTEXT — echoing them into the
    // commit message would leak every file name into the (unencrypted) git
    // history, defeating the name encryption. Count only.
    if redact_paths {
        return format!("git-drive: {} changes", n);
    }
    let paths: Vec<&str> = ops.iter().map(|op| op.primary_path()).collect();
    let preview: Vec<&str> = paths.iter().take(3).copied().collect();
    let more = if n > preview.len() {
        format!(", +{} more", n - preview.len())
    } else {
        String::new()
    };
    format!("git-drive: {} changes ({}{})", n, preview.join(", "), more)
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
            log::error!("chunk: error opening repo: {}", e);
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
            log::warn!(
                "chunk: {} sealed above the threshold — skipped (re-save the file)",
                rel
            );
            continue;
        }
        match convert_file_to_chunks(repo_path, &full, chunk_size, crypto) {
            Ok(manifest) => {
                converted += 1;
                log::info!(
                    "chunk: {} -> {} chunk(s) of up to {} bytes",
                    rel,
                    manifest.hashes.len(),
                    chunk_size
                );
            }
            Err(e) => log::error!("chunk: error converting {}: {}", rel, e),
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
    let tmp = full.with_extension("git-drive-manifest.tmp");
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
    let mut converted = repair_double_encrypted_conflicts(repo_path, c);
    for (full, rel) in walk_worktree_files(repo_path) {
        let leaf = rel.rsplit('/').next().unwrap_or(&rel);
        // Long-name sidecars are our own metadata, never user files.
        if crate::crypt::is_long_sidecar(leaf) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        // A component conforms when it is a hashed long entry OR decodes AND
        // stays within LONG_NAME_THRESHOLD — pre-threshold entries (encoded
        // 81..255, valid base32) must be re-written as dslong, or deep paths
        // keep breaking macOS's PATH_MAX on the replica (2026-07-10 incident).
        let comp_ok = |comp: &str| {
            let base = match c.dec_conflicted(comp) {
                // A conflicted copy conforms when its encrypted base does.
                Some((_, suffix)) => &comp[..comp.len() - suffix.len()],
                None => comp,
            };
            crate::crypt::is_long_name(comp)
                || (base.len() <= crate::crypt::LONG_NAME_THRESHOLD
                    && c.dec_component(base).is_some())
        };
        let name_ok = rel.split('/').all(comp_ok);
        if meta.file_type().is_symlink() {
            let Ok(target) = std::fs::read_link(&full) else { continue };
            let target_str = target.to_string_lossy().into_owned();
            let target_ok = target_str
                .split('/')
                .all(|comp| matches!(comp, "" | "." | "..") || comp_ok(comp));
            if name_ok && target_ok {
                continue;
            }
            let (new_rel, sidecars) = canonicalize_disk_path(c, &rel);
            let (new_target, _) = canonicalize_disk_path(c, &target_str);
            let new_full = repo_path.join(&new_rel);
            if let Some(parent) = new_full.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            materialize_sidecars(repo_path, &sidecars);
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
        let (new_rel, sidecars) = canonicalize_disk_path(c, &rel);
        let new_full = repo_path.join(&new_rel);
        if let Some(parent) = new_full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        materialize_sidecars(repo_path, &sidecars);
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
                let tmp = new_full.with_extension("git-drive-rechunk.tmp");
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
                    log::warn!(
                        "crypto: failed to expand cleartext manifest {:?} — skipped",
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
                    let tmp2 = new_full.with_extension("git-drive-crypt.tmp");
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
        let tmp = new_full.with_extension("git-drive-crypt.tmp");
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

/// Repairs conflicted copies whose name was encrypted TWICE by the old
/// migration sweep: `<encrypted>.conflict-S` was treated as cleartext and
/// became `dslong_<h>.conflict-S` with a sidecar holding enc(`<encrypted>`),
/// so the drive showed the base32 name of the original. Detected by the
/// sidecar decoding to a name that is itself a valid encrypted component;
/// renamed to the canonical form of `<clear>.conflict-S`. Idempotent.
fn repair_double_encrypted_conflicts(repo_path: &Path, c: &crate::crypt::Crypto) -> usize {
    let mut repaired = 0usize;
    for (full, rel) in walk_worktree_files(repo_path) {
        let (dir, leaf) = match rel.rsplit_once('/') {
            Some((d, l)) => (Some(d), l),
            None => (None, rel.as_str()),
        };
        if !crate::crypt::is_long_name(leaf) {
            continue;
        }
        let Some((_, suffix)) = crate::crypt::split_conflict(leaf) else {
            continue;
        };
        let join = |name: &str| match dir {
            Some(d) => repo_path.join(d).join(name),
            None => repo_path.join(name),
        };
        let exact_sidecar = join(&crate::crypt::sidecar_name(leaf));
        let Ok(content) = std::fs::read_to_string(&exact_sidecar) else {
            continue;
        };
        let Some(inner) = c.dec_component(content.trim()) else {
            continue;
        };
        // Double encryption: the "cleartext" is itself an encrypted name.
        let Some(real_base) = c.dec_component(&inner) else {
            continue;
        };
        let canonical = c.enc_component_keep_conflict(&format!("{}{}", real_base, suffix));
        let new_full = join(&canonical.disk);
        if let Some(sc) = &canonical.sidecar {
            let _ = std::fs::write(join(&crate::crypt::sidecar_name(&canonical.disk)), sc);
        }
        if new_full != full && std::fs::rename(&full, &new_full).is_ok() {
            let _ = std::fs::remove_file(&exact_sidecar);
            log::info!("crypto: repaired double-encrypted conflicted copy {}", rel);
            repaired += 1;
        }
    }
    repaired
}

/// `encrypted -> cleartext` migration: decrypts names and content; sealed
/// manifests are expanded back into the whole file (the chunk sweep
/// re-chunks it in cleartext right after, if still large).
fn migrate_worktree_to_clear(repo_path: &Path, mk: &crate::crypt::Crypto) -> usize {
    let mut converted = 0usize;
    // Long-name sidecars are decryption METADATA: they must survive until every
    // hashed (`dslong_`) entry has been resolved through them, and disappear
    // afterwards (a cleartext worktree has no sidecars). Deleting one before
    // its main entry is processed would leave the hash unresolvable.
    let mut sidecars_to_remove: Vec<PathBuf> = Vec::new();
    for (full, rel) in walk_worktree_files(repo_path) {
        let leaf = rel.rsplit('/').next().unwrap_or(&rel);
        if crate::crypt::is_long_sidecar(leaf) {
            sidecars_to_remove.push(full);
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        let clear_rel = dec_disk_path_with_worktree_sidecars(mk, repo_path, &rel);
        let name_cipher = clear_rel != rel;
        if meta.file_type().is_symlink() {
            let Ok(target) = std::fs::read_link(&full) else { continue };
            let clear_target = dec_disk_path_with_worktree_sidecars(
                mk,
                repo_path,
                &target.to_string_lossy(),
            );
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
            log::warn!(
                "crypto: failed to decrypt {:?} during migration — skipped",
                rel
            );
            continue;
        };
        if crate::chunk::Manifest::is_manifest_bytes(&plain) {
            // Expand: reassembles the whole file by decrypting chunk by
            // chunk (streaming to disk; RAM bounded by one chunk).
            let Ok(manifest) = crate::chunk::Manifest::parse(&plain) else { continue };
            let tmp = new_full.with_extension("git-drive-expand.tmp");
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
        let tmp = new_full.with_extension("git-drive-crypt.tmp");
        if std::fs::write(&tmp, &plain).is_ok() && std::fs::rename(&tmp, &new_full).is_ok() {
            if new_full != full {
                let _ = std::fs::remove_file(&full);
            }
            converted += 1;
        }
    }
    // Every hashed entry has been resolved by now — drop the sidecars (a
    // cleartext worktree carries no git-drive metadata).
    for sc in sidecars_to_remove {
        let _ = std::fs::remove_file(sc);
    }
    prune_empty_dirs(repo_path);
    converted
}

/// Decrypts an on-disk path, resolving hashed (`dslong_`) components by reading
/// their `<disk>.name` sidecars straight from the worktree. Components without
/// a resolvable sidecar stay literal, like every other mixed-state fallback.
pub(crate) fn dec_disk_path_with_worktree_sidecars(
    c: &crate::crypt::Crypto,
    repo_path: &Path,
    on_disk: &str,
) -> String {
    let mut clear: Vec<String> = Vec::new();
    let mut accum = String::new();
    for comp in on_disk.split('/') {
        if !accum.is_empty() {
            accum.push('/');
        }
        accum.push_str(comp);
        if crate::crypt::is_long_name(comp) {
            let sc = repo_path.join(crate::crypt::sidecar_name(&accum));
            match std::fs::read_to_string(&sc)
                .ok()
                .and_then(|ct| c.dec_long(comp, ct.trim()))
            {
                Some(cl) => clear.push(cl),
                None => clear.push(comp.to_string()),
            }
        } else {
            clear.push(c.dec_path(comp));
        }
    }
    clear.join("/")
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
                log::warn!(
                    "chunk: gc skipped — encrypted worktree without an available key"
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
                Ok(_) => log::debug!("chunk: gc removed orphan {}", name),
                Err(e) => log::error!("chunk: gc failed on {}: {}", name, e),
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

    fn write_op(path: &str, size: usize) -> SyncOp {
        SyncOp::Write {
            path: path.to_string(),
            content: wc(&vec![b'x'; size]),
        }
    }

    #[test]
    fn split_batches_zero_limit_keeps_single_batch() {
        let ops = vec![write_op("a", 10), write_op("b", 10)];
        let batches = split_batches(ops, 0);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2);
    }

    #[test]
    fn split_batches_cuts_by_accumulated_content() {
        let ops = vec![
            write_op("a", 60),
            write_op("b", 60), // 120 > 100 -> cut before b
            write_op("c", 30),
            write_op("d", 30), // 60+30+30... b(60)+c(30)=90, +d(30)=120 -> cut before d
        ];
        let batches = split_batches(ops, 100);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 1, "a alone (60)");
        assert_eq!(batches[1].len(), 2, "b + c (90)");
        assert_eq!(batches[2].len(), 1, "d");
        // Arrival order preserved across batches.
        let flat: Vec<&str> = batches
            .iter()
            .flatten()
            .map(|op| op.primary_path())
            .collect();
        assert_eq!(flat, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn split_batches_oversized_write_gets_own_batch() {
        let ops = vec![write_op("small", 10), write_op("huge", 500), write_op("tail", 10)];
        let batches = split_batches(ops, 100);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[1][0].primary_path(), "huge");
        assert_eq!(batches[1].len(), 1, "oversized write is alone in its batch");
    }

    #[test]
    fn split_batches_metadata_ops_are_free_and_never_cut() {
        let ops = vec![
            write_op("a", 90),
            SyncOp::Mkdir { path: "dir".into() },
            SyncOp::Rename {
                from: "a".into(),
                to: "dir/a".into(),
            },
            SyncOp::Delete { path: "old".into() },
        ];
        let batches = split_batches(ops, 100);
        assert_eq!(
            batches.len(),
            1,
            "metadata ops carry no content and ride with the current batch"
        );
        assert_eq!(batches[0].len(), 4);
    }

    #[test]
    fn apply_op_writes_and_removes_long_name_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let c = crate::crypt::Crypto::from_master_key(&[5u8; 32]);
        let clear = format!("{}.txt", "y".repeat(200));

        // Write: the on-disk file uses the hashed long name and gets a sidecar.
        apply_op_to_disk(
            repo,
            &SyncOp::Write {
                path: clear.clone(),
                content: wc(b"secret data"),
            },
            Some(&c),
            0,
        );
        let full = c.enc_path_full(&clear);
        assert_eq!(full.sidecars.len(), 1);
        let disk_file = repo.join(&full.disk);
        let sidecar = repo.join(&full.sidecars[0].path);
        assert!(disk_file.exists(), "hashed long-name file must exist");
        assert!(disk_file
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("dslong_"));
        assert!(sidecar.exists(), "sidecar must be written");
        assert_eq!(
            std::fs::read_to_string(&sidecar).unwrap(),
            full.sidecars[0].content
        );
        // Content is sealed, never cleartext.
        let bytes = std::fs::read(&disk_file).unwrap();
        assert!(crate::crypt::Crypto::is_encrypted_bytes(&bytes));

        // Delete: removes the file AND its sidecar (no orphan left behind).
        apply_op_to_disk(repo, &SyncOp::Delete { path: clear.clone() }, Some(&c), 0);
        assert!(!disk_file.exists());
        assert!(!sidecar.exists(), "sidecar must be removed with the file");
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
        let m = build_commit_message(&ops, false);
        assert!(m.contains("3 changes"), "{}", m);
        assert!(m.contains("a.txt"));
    }

    #[test]
    fn build_commit_message_redacts_paths_in_encrypted_mode() {
        let ops_owned = [
            SyncOp::Write { path: "secret-name.txt".into(), content: wc(b"") },
            SyncOp::Delete { path: "other/secret.doc".into() },
        ];
        let ops: Vec<&SyncOp> = ops_owned.iter().collect();
        // Encrypted repo: cleartext queue paths must NOT leak into the
        // (unencrypted) commit message.
        let m = build_commit_message(&ops, true);
        assert_eq!(m, "git-drive: 2 changes");
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
        let m = build_commit_message(&ops, false);
        assert!(m.contains("5 changes"), "{}", m);
        assert!(m.contains("+2 more"), "{}", m);
    }

    #[test]
    fn migration_rewrites_pre_threshold_long_components_to_dslong() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let c = crate::crypt::Crypto::from_master_key(&[9u8; 32]);

        // Fabricate an OLD-FORMAT worktree entry: a directly-encoded (valid
        // base32) component ABOVE the threshold, holding sealed content —
        // exactly what the pre-threshold binary wrote for medium-length names.
        let clear_name = "wordpress-uploads-backup-guard.jpg"; // 34 bytes -> ~119 encoded
        let old_disk = c.enc_component_unbounded(clear_name);
        assert!(old_disk.len() > crate::crypt::LONG_NAME_THRESHOLD);
        std::fs::write(repo.join(&old_disk), c.seal(b"payload")).unwrap();

        // An already-conforming short entry must be left alone.
        let short = c.enc_path("ok.txt");
        std::fs::write(repo.join(&short), c.seal(b"fine")).unwrap();

        let converted = migrate_worktree_to_encrypted(repo, &c);
        assert_eq!(converted, 1, "only the over-threshold entry migrates");

        // Old entry is gone; its dslong replacement + sidecar exist.
        assert!(!repo.join(&old_disk).exists());
        let e = c.enc_component(clear_name);
        assert!(e.disk.starts_with(crate::crypt::LONG_NAME_PREFIX));
        let new_full = repo.join(&e.disk);
        assert!(new_full.exists(), "dslong replacement must exist");
        let sc = repo.join(crate::crypt::sidecar_name(&e.disk));
        assert!(sc.exists(), "sidecar must exist");
        assert_eq!(
            c.dec_long(&e.disk, &std::fs::read_to_string(&sc).unwrap()).unwrap(),
            clear_name
        );
        // Content untouched (rename, not re-seal).
        assert_eq!(c.open(&std::fs::read(&new_full).unwrap()).unwrap(), b"payload");

        // Idempotent: second pass converts nothing.
        assert_eq!(migrate_worktree_to_encrypted(repo, &c), 0);
    }

    #[test]
    fn migration_leaves_conflicted_copy_of_encrypted_name_alone() {
        // A merge appends `.conflict-...` to the ENCRYPTED name on disk. The
        // sweep must treat that as conforming — it used to encrypt the base32
        // name a second time (2026-10-13 sandbox run).
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let c = crate::crypt::Crypto::from_master_key(&[9u8; 32]);
        let short = c.enc_component("shared.txt").disk; // exactly 80 chars
        assert!(short.len() <= crate::crypt::LONG_NAME_THRESHOLD);
        let copy = format!("{}.conflict-host-1791868058", short);
        std::fs::write(repo.join(&copy), c.seal(b"local version")).unwrap();

        assert_eq!(migrate_worktree_to_encrypted(repo, &c), 0);
        assert!(repo.join(&copy).exists(), "conflicted copy untouched");
        assert_eq!(
            c.dec_path(&copy),
            "shared.txt.conflict-host-1791868058",
            "still decodes to the clear name"
        );
    }

    #[test]
    fn migration_repairs_double_encrypted_conflicted_copy() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let c = crate::crypt::Crypto::from_master_key(&[9u8; 32]);
        let suffix = ".conflict-host-1791868058";
        // Reproduce the damage: the encrypted base treated as cleartext.
        let enc_base = c.enc_component("shared.txt").disk;
        let bad = c.enc_component(&enc_base);
        assert!(bad.disk.starts_with(crate::crypt::LONG_NAME_PREFIX));
        let bad_leaf = format!("{}{}", bad.disk, suffix);
        std::fs::write(repo.join(&bad_leaf), c.seal(b"local version")).unwrap();
        std::fs::write(
            repo.join(crate::crypt::sidecar_name(&bad_leaf)),
            bad.sidecar.clone().unwrap(),
        )
        .unwrap();

        migrate_worktree_to_encrypted(repo, &c);

        let good = format!("{}{}", enc_base, suffix);
        assert!(repo.join(&good).exists(), "renamed to the canonical conflicted copy");
        assert!(!repo.join(&bad_leaf).exists());
        assert!(!repo.join(crate::crypt::sidecar_name(&bad_leaf)).exists());
        assert_eq!(c.dec_path(&good), format!("shared.txt{}", suffix));
        assert_eq!(c.open(&std::fs::read(repo.join(&good)).unwrap()).unwrap(), b"local version");
        assert_eq!(migrate_worktree_to_encrypted(repo, &c), 0, "idempotent");
    }

    #[test]
    fn canonicalize_preserves_dslong_parents_verbatim() {
        let c = crate::crypt::Crypto::from_master_key(&[9u8; 32]);
        // A path whose parent is ALREADY a hashed entry and whose leaf is an
        // old-format over-threshold component: the parent must pass through
        // untouched (re-encrypting the literal hash would corrupt the entry).
        let parent = c.enc_component(&"p".repeat(40)).disk;
        assert!(parent.starts_with(crate::crypt::LONG_NAME_PREFIX));
        let leaf_old = c.enc_component_unbounded("some-medium-name.txt");
        let (new_rel, sidecars) =
            canonicalize_disk_path(&c, &format!("{}/{}", parent, leaf_old));
        let mut parts = new_rel.split('/');
        assert_eq!(parts.next().unwrap(), parent, "dslong parent untouched");
        let new_leaf = parts.next().unwrap();
        assert!(new_leaf.starts_with(crate::crypt::LONG_NAME_PREFIX));
        assert_eq!(sidecars.len(), 1);
        assert!(sidecars[0].0.starts_with(&format!("{}/", parent)));
    }
}
