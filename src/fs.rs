use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory,
    ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr,
    Request,
};
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use crate::git;
use crate::sync::SyncManager;

const TTL: Duration = Duration::from_secs(1);
const ROOT_INODE: u64 = 1;

/// Flags from macOS `renameatx_np(2)`, propagated by macFUSE in
/// `fuse_rename_in` (local patch in vendor/fuser). SWAP = atomic content
/// exchange; EXCL = fail if the destination exists.
const APPLE_RENAME_SWAP: u32 = 0x2;
const APPLE_RENAME_EXCL: u32 = 0x4;

/// (inode, path) entries from a subtree traversal.
type SubtreeEntries = Vec<(u64, String)>;
const FILE_MODE: u16 = 0o644;
const DIR_MODE: u16 = 0o755;
/// Default mode exposed for symlinks (lrwxr-xr-x, as macOS creates them).
const SYMLINK_MODE: u16 = 0o755;

/// LRU cache of read blobs, keyed by inode. Budget in bytes; a blob larger
/// than the whole budget is never admitted (reads are served straight from
/// the repo). Expected scale: dozens of entries for a personal mount — O(n)
/// eviction is invisible next to the repo-open cost each hit avoids.
pub(crate) struct BlobCache {
    entries: HashMap<u64, BlobCacheEntry>,
    budget_bytes: u64,
    total_bytes: u64,
    tick: u64,
    hits: u64,
    misses: u64,
}

struct BlobCacheEntry {
    data: Vec<u8>,
    last_access: u64,
}

impl BlobCache {
    pub(crate) fn new(budget_bytes: u64) -> Self {
        BlobCache {
            entries: HashMap::new(),
            budget_bytes,
            total_bytes: 0,
            tick: 0,
            hits: 0,
            misses: 0,
        }
    }

    fn get(&mut self, ino: u64) -> Option<&Vec<u8>> {
        self.tick += 1;
        let tick = self.tick;
        match self.entries.get_mut(&ino) {
            Some(entry) => {
                entry.last_access = tick;
                self.hits += 1;
                Some(&entry.data)
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    fn insert(&mut self, ino: u64, data: Vec<u8>) {
        let len = data.len() as u64;
        if len > self.budget_bytes {
            return;
        }
        self.remove(ino);
        while self.total_bytes + len > self.budget_bytes {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_access)
                .map(|(ino, _)| *ino)
            else {
                break;
            };
            self.remove(oldest);
        }
        self.tick += 1;
        self.total_bytes += len;
        self.entries.insert(
            ino,
            BlobCacheEntry {
                data,
                last_access: self.tick,
            },
        );
    }

    fn remove(&mut self, ino: u64) {
        if let Some(entry) = self.entries.remove(&ino) {
            self.total_bytes -= entry.data.len() as u64;
        }
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }

    fn contains(&self, ino: u64) -> bool {
        self.entries.contains_key(&ino)
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

/// Per-inode write buffer. Below the spool threshold it lives in RAM with
/// copy-on-write mutation (`Arc` shared with the sync queue); above it, in a
/// spool file inside `.git/` — peak RAM bounded by the threshold.
pub(crate) enum WriteBuffer {
    Mem(Arc<Vec<u8>>),
    Spooled(SpooledBuffer),
}

/// Spool file with random access. Removed from disk on drop (buffer gc,
/// unlink, shutdown).
pub(crate) struct SpooledBuffer {
    file: std::fs::File,
    path: std::path::PathBuf,
    len: u64,
}

impl Drop for SpooledBuffer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl WriteBuffer {
    fn len(&self) -> u64 {
        match self {
            WriteBuffer::Mem(a) => a.len() as u64,
            WriteBuffer::Spooled(s) => s.len,
        }
    }

    fn read_range(&self, offset: usize, size: usize) -> Vec<u8> {
        match self {
            WriteBuffer::Mem(a) => {
                let data = a.as_slice();
                let start = offset.min(data.len());
                let end = offset.saturating_add(size).min(data.len());
                data[start..end].to_vec()
            }
            WriteBuffer::Spooled(s) => {
                use std::os::unix::fs::FileExt;
                let start = (offset as u64).min(s.len);
                let end = (offset as u64).saturating_add(size as u64).min(s.len);
                let mut out = vec![0u8; (end - start) as usize];
                if s.file.read_exact_at(&mut out, start).is_err() {
                    out.clear();
                }
                out
            }
        }
    }

    fn full_vec(&self) -> Vec<u8> {
        self.read_range(0, self.len() as usize)
    }

    fn write_at(&mut self, offset: usize, data: &[u8]) {
        match self {
            WriteBuffer::Mem(a) => {
                // Copy-on-write: if a queued op still holds this Arc,
                // make_mut clones before mutating — the op keeps its snapshot.
                let buf = Arc::make_mut(a);
                if buf.len() < offset + data.len() {
                    buf.resize(offset + data.len(), 0);
                }
                buf[offset..offset + data.len()].copy_from_slice(data);
            }
            WriteBuffer::Spooled(s) => {
                use std::os::unix::fs::FileExt;
                // Writing past EOF extends with zeros (hole), the same
                // semantics as the resize on the RAM path.
                let _ = s.file.write_all_at(data, offset as u64);
                s.len = s.len.max((offset + data.len()) as u64);
            }
        }
    }

    fn set_len(&mut self, n: u64) {
        match self {
            WriteBuffer::Mem(a) => Arc::make_mut(a).resize(n as usize, 0),
            WriteBuffer::Spooled(s) => {
                let _ = s.file.set_len(n);
                s.len = n;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FileEntry {
    pub(crate) inode: u64,
    pub(crate) path: String,
    pub(crate) parent: u64,
    pub(crate) size: u64,
    /// Regular or Symlink (filemode 120000 in Git). For a symlink, the blob
    /// content is the target text and the attr exposes `FileType::Symlink`.
    pub(crate) kind: git::FileKind,
    /// File matching the mounted repo's ignore rules: lives in the working
    /// tree, never enters a commit. Reads come from disk, not from HEAD.
    pub(crate) local_only: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DirEntry {
    pub(crate) inode: u64,
    pub(crate) path: String,
    pub(crate) parent: u64,
    pub(crate) children: HashSet<u64>,
}

/// In-memory extended attributes: inode -> (name -> value).
type XattrMap = HashMap<u64, HashMap<OsString, Vec<u8>>>;

pub struct GitDriveFS {
    repo_path: String,
    #[allow(dead_code)]
    branch: String,
    files_by_inode: HashMap<u64, FileEntry>,
    dirs_by_inode: HashMap<u64, DirEntry>,
    inodes_by_path: HashMap<String, u64>,
    next_inode: Arc<AtomicU64>,
    sync: SyncManager,
    /// Current content per inode: copy-on-write RAM (shared with the queue
    /// via `Arc`) below the spool threshold, file on disk above it.
    write_buffers: Arc<parking_lot::Mutex<HashMap<u64, WriteBuffer>>>,
    /// Threshold in bytes for spooling buffers (0 = spool disabled).
    spool_threshold_bytes: u64,
    /// Directory for spool files (inside `.git/` — same volume as the
    /// working tree and invisible to the commit). Cleared at boot.
    spool_dir: std::path::PathBuf,
    spool_counter: AtomicU64,
    /// Inodes with writes not yet queued into sync ("dirty"). Flush only
    /// emits `SyncOp::Write` for marked inodes — flush runs on every close(2),
    /// including read-only fds, and without the mark it would re-queue stale
    /// content (potentially reverting merged remote changes).
    dirty: HashSet<u64>,
    /// Last observed `commit_generation` of the `SyncManager`. When it
    /// advances, clean (non-dirty) buffers are discarded — their content is
    /// already in HEAD and keeping them would hide future remote changes
    /// from `read`.
    last_commit_gen: u64,
    /// LRU cache of read blobs. Invalidated wholesale at the same points as
    /// the buffer gc (HEAD changed); per inode on unlink/write/
    /// truncate/rename.
    read_cache: BlobCache,
    /// Repo handle reused on the hot read path (cache miss). Without it, a
    /// file larger than the budget reopened the repo on EVERY read syscall.
    /// HEAD is re-resolved on each use (refs come from disk), so the handle
    /// never serves a stale commit.
    read_repo: Option<git2::Repository>,
    /// Counter of actual handle opens (test introspection).
    read_repo_opens: u64,
    /// Encryption at rest (Phase 2.7): `Some` = encrypted repo, clear view.
    crypto: Option<Arc<crate::crypt::Crypto>>,
    uid: u32,
    gid: u32,
    mode_overrides: Arc<parking_lot::Mutex<HashMap<u64, u16>>>,
    times: Arc<parking_lot::Mutex<HashMap<u64, SystemTime>>>,
    /// mtime derived from the last commit that touched each path (single
    /// revwalk at boot/refresh). Consulted after `times` (session) and before
    /// `boot_time` (residue: local-only and directories).
    derived_times: HashMap<u64, SystemTime>,
    /// Encrypted mode: on-disk path -> clear path of every indexed entry.
    /// The mapping is deterministic, so refreshes reuse it and only decode
    /// (and read the sidecars of) paths they have not seen — re-decoding all
    /// ~100k names on every post-pull refresh froze the drive for seconds.
    name_cache: HashMap<String, String>,
    /// HEAD the index was built from; lets the post-pull refresh recompute
    /// mtimes only for the paths the new commits touched.
    indexed_head: Option<git2::Oid>,
    /// Lazy per-inode manifest detection cache (Phase 2.6): `Some` =
    /// chunked file (parsed manifest), `None` = checked and not chunked.
    /// Cleared together with the buffer gc (HEAD changed).
    manifests: parking_lot::Mutex<HashMap<u64, Option<Arc<crate::chunk::Manifest>>>>,
    /// Verified open handles of the working-tree files that `read` serves by
    /// range (chunks): each file is checked once, then only the requested
    /// bytes are read and decrypted.
    range: crate::range_read::RangeReader,
    /// Blob id at HEAD of each large file served by range (cleared when
    /// HEAD changes) and the inodes already warned about falling back to the
    /// object store.
    head_blob_ids: HashMap<u64, git2::Oid>,
    range_fallbacks: HashSet<u64>,
    boot_time: SystemTime,
    xattrs: Arc<parking_lot::Mutex<XattrMap>>,
}

/// Regular files at least this large are read by range from the working
/// tree; smaller ones go through the blob cache (whole-blob loads are cheap
/// there and the cache serves repeated small reads from RAM).
const RANGE_READ_MIN_BYTES: u64 = 4 * 1024 * 1024;

/// Open working-tree handles kept by range reads (LRU). Bounds file
/// descriptors; a handle evicted is reopened and checked again on demand.
const RANGE_READ_HANDLES: usize = 64;

/// Default read cache budget when the caller does not specify one
/// (mirrors the `read_cache_mb` default in the config).
const DEFAULT_READ_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// Default write buffer spool threshold (mirrors the
/// `large_file_threshold_mb` default in the config).
const DEFAULT_SPOOL_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;

impl GitDriveFS {
    pub fn new(repo_path: &str, branch: String, sync: SyncManager) -> anyhow::Result<Self> {
        Self::new_with_cache_budget(repo_path, branch, sync, DEFAULT_READ_CACHE_BYTES)
    }

    pub fn new_with_cache_budget(
        repo_path: &str,
        branch: String,
        sync: SyncManager,
        read_cache_bytes: u64,
    ) -> anyhow::Result<Self> {
        Self::new_with_options(
            repo_path,
            branch,
            sync,
            read_cache_bytes,
            DEFAULT_SPOOL_THRESHOLD_BYTES,
        )
    }

    pub fn new_with_options(
        repo_path: &str,
        branch: String,
        sync: SyncManager,
        read_cache_bytes: u64,
        spool_threshold_bytes: u64,
    ) -> anyhow::Result<Self> {
        Self::new_with_options_crypto(
            repo_path,
            branch,
            sync,
            read_cache_bytes,
            spool_threshold_bytes,
            None,
        )
    }

    pub fn new_with_options_crypto(
        repo_path: &str,
        branch: String,
        sync: SyncManager,
        read_cache_bytes: u64,
        spool_threshold_bytes: u64,
        crypto: Option<Arc<crate::crypt::Crypto>>,
    ) -> anyhow::Result<Self> {
        let t0 = std::time::Instant::now();
        // Names are decrypted from sidecars in the WORKING TREE: a pull
        // rewriting it meanwhile made a directory and a file under it decode
        // differently, and the boot panicked on the orphan file (Mac drive,
        // 2026-10-14). The repo lock keeps the pull and the drain out until
        // the listing is consistent.
        let repo_guard = sync.lock_repo();
        let repo = git::open_repo(repo_path)?;
        let commit = git::get_commit(&repo, "HEAD")?;
        let dirs = git::list_dirs_at_commit(&repo, &commit)?;
        let files = git::list_files_at_commit(&repo, &commit)?;
        let indexed_head = Some(commit.id());
        let t_list = t0.elapsed();
        let (dirs, files, name_cache) =
            decrypt_listing_cached(crypto.as_deref(), repo_path, dirs, files, &HashMap::new());
        drop(repo_guard);
        let t_decrypt = t0.elapsed();
        let n_files = files.len();

        let mut files_by_inode = HashMap::new();
        let mut dirs_by_inode = HashMap::new();
        let mut inodes_by_path = HashMap::new();

        let root = DirEntry {
            inode: ROOT_INODE,
            path: String::new(),
            parent: 0,
            children: HashSet::new(),
        };
        dirs_by_inode.insert(ROOT_INODE, root);
        inodes_by_path.insert(String::new(), ROOT_INODE);

        let next_inode = Arc::new(AtomicU64::new(2));

        // Process directories in increasing depth order to guarantee the
        // parent exists when the child is inserted.
        let mut dirs_sorted = dirs;
        dirs_sorted.sort_by_key(|p| p.matches('/').count());

        for dir_path in dirs_sorted {
            // `.chunks` is an internal chunking artifact (Phase 2.6) — invisible.
            if crate::reserved::is_reserved_path(&dir_path) {
                continue;
            }
            let inode = next_inode.fetch_add(1, Ordering::SeqCst);
            let parent_path = match dir_path.rfind('/') {
                Some(i) => dir_path[..i].to_string(),
                None => String::new(),
            };
            let parent_inode = *inodes_by_path
                .get(&parent_path)
                .expect("parent must exist (dirs sorted by depth)");
            let entry = DirEntry {
                inode,
                path: dir_path.clone(),
                parent: parent_inode,
                children: HashSet::new(),
            };
            dirs_by_inode.insert(inode, entry);
            inodes_by_path.insert(dir_path.clone(), inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        for git::ListedFile { path: file_path, size, kind } in files {
            // `.gitkeep` is an internal artifact of the "Git does not track
            // empty dirs" illusion (Phase 2, decision 8): it never enters the
            // indexes nor shows up in readdir — without this, rmdir of an
            // "empty" folder failed with ENOTEMPTY after a refresh (finding
            // from the 2026-07-03 validation).
            if leaf_name(&file_path) == ".gitkeep" || crate::reserved::is_reserved_path(&file_path) {
                continue;
            }
            let inode = next_inode.fetch_add(1, Ordering::SeqCst);
            let parent_path = match file_path.rfind('/') {
                Some(i) => file_path[..i].to_string(),
                None => String::new(),
            };
            let parent_inode = *inodes_by_path
                .get(&parent_path)
                .expect("file parent must exist");
            let entry = FileEntry {
                inode,
                path: file_path.clone(),
                parent: parent_inode,
                size,
                kind,
                local_only: false,
            };
            files_by_inode.insert(inode, entry);
            inodes_by_path.insert(file_path, inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        // Large buffer spool: leftovers from a previous crash are garbage.
        // The pre-rename dir (drive-sync-spool) is the same garbage.
        let _ = std::fs::remove_dir_all(std::path::Path::new(repo_path).join(".git/drive-sync-spool"));
        let spool_dir = std::path::Path::new(repo_path).join(".git/git-drive-spool");
        let _ = std::fs::remove_dir_all(&spool_dir);
        let _ = std::fs::create_dir_all(&spool_dir);

        let mut fs = GitDriveFS {
            repo_path: repo_path.to_string(),
            branch,
            files_by_inode,
            dirs_by_inode,
            inodes_by_path,
            next_inode,
            sync,
            write_buffers: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            spool_threshold_bytes,
            spool_dir,
            spool_counter: AtomicU64::new(0),
            dirty: HashSet::new(),
            last_commit_gen: 0,
            read_cache: BlobCache::new(read_cache_bytes),
            read_repo: None,
            read_repo_opens: 0,
            crypto,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            mode_overrides: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            times: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            derived_times: HashMap::new(),
            name_cache,
            indexed_head,
            manifests: parking_lot::Mutex::new(HashMap::new()),
            range: crate::range_read::RangeReader::new(RANGE_READ_HANDLES),
            head_blob_ids: HashMap::new(),
            range_fallbacks: HashSet::new(),
            boot_time: SystemTime::now(),
            xattrs: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        };
        let t_index = t0.elapsed();
        // Ignored files present in the working tree enter as visible
        // local-only entries (Phase 2.5): shown in the mount, never committed.
        fs.incorporate_local_only(&HashMap::new());
        let t_local = t0.elapsed();
        // mtime derived from history: without this, every remount would
        // "renew" the mtime of every file to the boot instant.
        fs.refresh_derived_times();
        log::info!(
            "boot: index of {} files built in {:.1?} (list {:.1?}, names {:.1?}, index {:.1?}, local-only {:.1?}, mtimes {:.1?})",
            n_files,
            t0.elapsed(),
            t_list,
            t_decrypt - t_list,
            t_index - t_decrypt,
            t_local - t_index,
            t0.elapsed() - t_local
        );
        // The listing, the decryption buffers and the mtime revwalk are gone.
        crate::release_freed_memory();
        Ok(fs)
    }

    fn perm_for(&self, ino: u64, default: u16) -> u16 {
        self.mode_overrides
            .lock()
            .get(&ino)
            .copied()
            .unwrap_or(default)
    }

    fn time_for(&self, ino: u64) -> SystemTime {
        if let Some(t) = self.times.lock().get(&ino) {
            return *t;
        }
        self.derived_times
            .get(&ino)
            .copied()
            .unwrap_or(self.boot_time)
    }

    fn touch(&self, ino: u64) {
        self.times.lock().insert(ino, SystemTime::now());
    }

    fn file_attr_for(&self, ino: u64, size: u64) -> FileAttr {
        let (ftype, default_mode) = match self.files_by_inode.get(&ino).map(|f| f.kind) {
            Some(git::FileKind::Symlink) => (FileType::Symlink, SYMLINK_MODE),
            _ => (FileType::RegularFile, FILE_MODE),
        };
        file_attr(
            ino,
            size,
            self.perm_for(ino, default_mode),
            self.uid,
            self.gid,
            self.time_for(ino),
            ftype,
        )
    }

    fn dir_attr_for(&self, ino: u64) -> FileAttr {
        dir_attr(
            ino,
            self.perm_for(ino, DIR_MODE),
            self.uid,
            self.gid,
            self.time_for(ino),
        )
    }

    fn alloc_inode(&self) -> u64 {
        self.next_inode.fetch_add(1, Ordering::SeqCst)
    }

    fn resolve_parent_path(path: &str) -> String {
        match path.rfind('/') {
            Some(i) => path[..i].to_string(),
            None => String::new(),
        }
    }

    fn resolve_parent_inode(&self, path: &str) -> Option<u64> {
        let parent_path = Self::resolve_parent_path(path);
        self.inodes_by_path.get(&parent_path).copied()
    }

    // ---- test introspection ----
    pub fn file_count(&self) -> usize {
        self.files_by_inode.len()
    }

    pub fn dir_count(&self) -> usize {
        // root does not count
        self.dirs_by_inode.len() - 1
    }

    pub fn find_inode(&self, path: &str) -> Option<u64> {
        self.inodes_by_path.get(path).copied()
    }

    pub fn alloc_inode_for_test(&self) -> u64 {
        self.alloc_inode()
    }

    pub fn resolve_parent_for_test(&self, path: &str) -> Option<u64> {
        self.resolve_parent_inode(path)
    }

    /// Resolves the child inode of `parent` by name. `None` if absent.
    fn find_child_inode(&self, parent: u64, name: &str) -> Option<u64> {
        let dir = self.dirs_by_inode.get(&parent)?;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    return Some(*child);
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    return Some(*child);
                }
            }
        }
        None
    }

    /// Reads a file's current content: first from the `write_buffer` (writes
    /// not yet drained), then from the blob at Git HEAD.
    fn current_file_content(&self, ino: u64) -> Option<Vec<u8>> {
        let entry = self.files_by_inode.get(&ino)?;
        let path = entry.path.clone();
        self.content_for_inode_at_path(ino, &path)
    }

    /// Explicit version: consults the write buffer (keyed by inode) and, if
    /// empty, reads the HEAD blob at the given path. Useful when `apply_rename`
    /// has already changed the in-memory path but still needs the old content.
    fn content_for_inode_at_path(&self, ino: u64, path: &str) -> Option<Vec<u8>> {
        if let Some(buf) = self.write_buffers.lock().get(&ino) {
            return Some(buf.full_vec());
        }
        if let Some(f) = self.files_by_inode.get(&ino) {
            if f.local_only {
                return self.read_local_only_content(f.kind, path);
            }
        }
        if let Some(m) = self.manifest_for(ino) {
            return self.reassemble_chunked(&m);
        }
        let repo = git::open_repo(&self.repo_path).ok()?;
        let commit = git::get_commit(&repo, "HEAD").ok()?;
        let bytes = git::read_file_at_commit(&repo, &commit, &self.disk_path(path)).ok()?;
        self.maybe_open(bytes).ok()
    }

    // ---- pure logic functions for integration tests ----

    pub fn apply_lookup(&self, parent: u64, name: &str) -> Result<FileAttr, i32> {
        let dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    return Ok(self.file_attr_for(f.inode, self.effective_size(f.inode, f.size)));
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    return Ok(self.dir_attr_for(d.inode));
                }
            }
        }
        Err(libc::ENOENT)
    }

    pub fn apply_getattr(&self, ino: u64) -> Result<FileAttr, i32> {
        if let Some(d) = self.dirs_by_inode.get(&ino) {
            Ok(self.dir_attr_for(d.inode))
        } else if let Some(f) = self.files_by_inode.get(&ino) {
            Ok(self.file_attr_for(f.inode, self.effective_size(f.inode, f.size)))
        } else {
            Err(libc::ENOENT)
        }
    }

    /// Creates a fresh spool file with the given content. `None` on I/O
    /// error — the caller degrades to RAM.
    fn spool_from_slice(&self, data: &[u8]) -> Option<SpooledBuffer> {
        let n = self.spool_counter.fetch_add(1, Ordering::SeqCst);
        let path = self.spool_dir.join(format!("buf-{}", n));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .ok()?;
        use std::os::unix::fs::FileExt;
        file.write_all_at(data, 0).ok()?;
        Some(SpooledBuffer {
            file,
            path,
            len: data.len() as u64,
        })
    }

    /// Builds a new buffer: spooled if the content is born above the threshold.
    fn make_buffer_from_vec(&self, base: Vec<u8>) -> WriteBuffer {
        if self.spool_threshold_bytes > 0 && base.len() as u64 >= self.spool_threshold_bytes {
            if let Some(sb) = self.spool_from_slice(&base) {
                return WriteBuffer::Spooled(sb);
            }
        }
        WriteBuffer::Mem(Arc::new(base))
    }

    /// Migrates a RAM buffer to spool when it crosses the threshold.
    fn maybe_spill(&self, buf: &mut WriteBuffer) {
        if self.spool_threshold_bytes == 0 {
            return;
        }
        if let WriteBuffer::Mem(a) = buf {
            if a.len() as u64 >= self.spool_threshold_bytes {
                if let Some(sb) = self.spool_from_slice(a) {
                    *buf = WriteBuffer::Spooled(sb);
                }
            }
        }
    }

    /// Immutable buffer snapshot for the sync queue. RAM: cloned `Arc`
    /// (zero copy). Spooled: disk-to-disk copy into a queue file — later
    /// writes to the buffer must not mutate the causal snapshot.
    fn snapshot_buffer_content(&self, buf: &WriteBuffer) -> crate::sync::WriteContent {
        match buf {
            WriteBuffer::Mem(a) => crate::sync::WriteContent::Mem(a.clone()),
            WriteBuffer::Spooled(s) => {
                let n = self.spool_counter.fetch_add(1, Ordering::SeqCst);
                let qpath = self.spool_dir.join(format!("q-{}", n));
                match std::fs::copy(&s.path, &qpath) {
                    Ok(_) => crate::sync::WriteContent::Spooled(qpath),
                    Err(_) => crate::sync::WriteContent::Mem(Arc::new(buf.full_vec())),
                }
            }
        }
    }

    // ---- Phase 2.6: chunked files (manifest + .chunks) ----

    /// Inode manifest, with cached lazy detection (`Some` = chunked).
    fn manifest_for(&self, ino: u64) -> Option<Arc<crate::chunk::Manifest>> {
        if let Some(cached) = self.manifests.lock().get(&ino) {
            return cached.clone();
        }
        let candidate = match self.files_by_inode.get(&ino) {
            Some(f)
                if f.kind == git::FileKind::Regular
                    && !f.local_only
                    && f.size > 0
                    && f.size <= crate::chunk::MANIFEST_MAX_BYTES =>
            {
                Some(f.path.clone())
            }
            _ => None,
        };
        let result = candidate.and_then(|path| {
            let repo = git::open_repo(&self.repo_path).ok()?;
            let commit = git::get_commit(&repo, "HEAD").ok()?;
            let bytes =
                git::read_file_at_commit(&repo, &commit, &self.disk_path(&path)).ok()?;
            let bytes = self.maybe_open(bytes).ok()?;
            if !crate::chunk::Manifest::is_manifest_bytes(&bytes) {
                return None;
            }
            crate::chunk::Manifest::parse(&bytes).ok().map(Arc::new)
        });
        self.manifests.lock().insert(ino, result.clone());
        result
    }

    /// Size visible to userspace: local buffer wins; otherwise the real size
    /// from the manifest; otherwise the raw size from the index.
    fn effective_size(&self, ino: u64, raw: u64) -> u64 {
        if self.write_buffers.lock().contains_key(&ino) {
            return raw;
        }
        self.manifest_for(ino).map(|m| m.total_size).unwrap_or(raw)
    }

    /// Reads and verifies a chunk from the working tree. Hash mismatch = EIO —
    /// never silently wrong content.
    fn read_verified_chunk(
        &self,
        m: &crate::chunk::Manifest,
        idx: usize,
    ) -> Result<Vec<u8>, i32> {
        let hash = m.hashes.get(idx).ok_or(libc::EIO)?;
        let full =
            std::path::Path::new(&self.repo_path).join(crate::chunk::shard_rel_path(hash));
        let data = std::fs::read(&full).map_err(|_| libc::EIO)?;
        if crate::chunk::sha256_hex(&data) != *hash {
            log::error!(
                "chunk: hash mismatch on chunk {} (idx {}) — EIO",
                hash,
                idx
            );
            return Err(libc::EIO);
        }
        // With encryption, the hash covers the CIPHERTEXT; decryption comes after.
        self.maybe_open(data)
    }

    /// Reassembled read of a chunked file: per chunk, only the requested
    /// range is read (and decrypted) from the chunk file, verified against
    /// the manifest hash once per session. Interleaved readers on different
    /// chunks no longer reload whole chunks.
    fn read_chunked(
        &mut self,
        m: &crate::chunk::Manifest,
        offset: usize,
        size: usize,
    ) -> Result<Vec<u8>, i32> {
        use crate::range_read::{Expect, RangeError};
        let mut out = Vec::new();
        let end = (offset as u64).saturating_add(size as u64).min(m.total_size);
        for idx in m.chunks_for_range(offset as u64, size as u64) {
            let hash = &m.hashes[idx];
            let chunk_start = idx as u64 * m.chunk_size;
            let chunk_len = m.chunk_size.min(m.total_size - chunk_start);
            let s = (offset as u64).max(chunk_start) - chunk_start;
            let e = end.min(chunk_start + chunk_len) - chunk_start;
            let path =
                std::path::Path::new(&self.repo_path).join(crate::chunk::shard_rel_path(hash));
            let read = self.range.read(
                &path,
                &Expect::Sha256(hash.clone()),
                self.crypto.as_deref(),
                Some(chunk_len),
                s,
                (e - s) as usize,
            );
            match read {
                Ok((data, _)) if data.len() as u64 == e - s => out.extend_from_slice(&data),
                Ok(_) | Err(RangeError::Io) => {
                    log::error!("chunk: cannot read chunk {} (idx {}) — EIO", hash, idx);
                    return Err(libc::EIO);
                }
                Err(RangeError::Mismatch) => {
                    log::error!("chunk: hash mismatch on chunk {} (idx {}) — EIO", hash, idx);
                    return Err(libc::EIO);
                }
                Err(RangeError::Corrupt) => {
                    log::error!(
                        "chunk: chunk {} (idx {}) does not fit its manifest — EIO",
                        hash,
                        idx
                    );
                    return Err(libc::EIO);
                }
            }
        }
        Ok(out)
    }

    /// Fully reassembled content (cold paths: swap, link, fallback).
    fn reassemble_chunked(&self, m: &crate::chunk::Manifest) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(m.total_size as usize);
        for idx in 0..m.hashes.len() {
            out.extend_from_slice(&self.read_verified_chunk(m, idx).ok()?);
        }
        Some(out)
    }

    /// Write buffer for a chunked file: reassembles chunk by chunk straight
    /// into the spool when the total exceeds the threshold — RAM bounded by
    /// one chunk.
    fn buffer_from_manifest(&self, m: &crate::chunk::Manifest) -> Option<WriteBuffer> {
        if self.spool_threshold_bytes > 0 && m.total_size >= self.spool_threshold_bytes {
            let n = self.spool_counter.fetch_add(1, Ordering::SeqCst);
            let path = self.spool_dir.join(format!("buf-{}", n));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
                .ok()?;
            use std::os::unix::fs::FileExt;
            let mut off: u64 = 0;
            for idx in 0..m.hashes.len() {
                let data = self.read_verified_chunk(m, idx).ok()?;
                file.write_all_at(&data, off).ok()?;
                off += data.len() as u64;
            }
            Some(WriteBuffer::Spooled(SpooledBuffer { file, path, len: off }))
        } else {
            self.reassemble_chunked(m)
                .map(|v| self.make_buffer_from_vec(v))
        }
    }

    /// Ensures the inode has a write buffer, initializing it with the file's
    /// current content (blob at HEAD) on the first mutation.
    /// POSIX semantics: `write`/`truncate` operate on the existing content —
    /// a buffer born empty zeroed out everything not rewritten (corruption on
    /// append, editor dirty pages and truncate).
    fn ensure_buffer(&self, ino: u64, path: &str) {
        if self.write_buffers.lock().contains_key(&ino) {
            return;
        }
        let buffer = if let Some(m) = self.manifest_for(ino) {
            // Chunked: the buffer is born from the REASSEMBLY, never from the
            // raw manifest — same POSIX semantics as the Phase 0 invariant.
            self.buffer_from_manifest(&m).unwrap_or_else(|| {
                log::warn!("chunk: failed to reassemble write buffer — starting empty");
                WriteBuffer::Mem(Arc::new(Vec::new()))
            })
        } else {
            let base = self.content_for_inode_at_path(ino, path).unwrap_or_default();
            self.make_buffer_from_vec(base)
        };
        self.write_buffers.lock().entry(ino).or_insert(buffer);
    }

    /// Write at an offset into the inode's buffer. Returns bytes accepted.
    pub fn apply_write(&mut self, ino: u64, offset: usize, data: &[u8]) -> Result<u32, i32> {
        let path = match self.files_by_inode.get(&ino) {
            Some(f) => f.path.clone(),
            None => return Err(libc::ENOENT),
        };
        self.ensure_buffer(ino, &path);
        let new_len = {
            let mut buffers = self.write_buffers.lock();
            let buf = buffers.get_mut(&ino).expect("buffer guaranteed above");
            buf.write_at(offset, data);
            self.maybe_spill(buf);
            buf.len()
        };
        if let Some(f) = self.files_by_inode.get_mut(&ino) {
            f.size = new_len;
        }
        self.dirty.insert(ino);
        // The buffer now takes precedence on read; the cache entry would
        // only waste memory (and go stale after the commit).
        self.read_cache.remove(ino);
        Ok(data.len() as u32)
    }

    /// Truncate (setattr with `size`). Materializes the op immediately — a
    /// path-based `truncate(2)` may not be followed by a flush.
    pub fn apply_truncate(&mut self, ino: u64, new_size: u64) -> Result<(), i32> {
        let path = match self.files_by_inode.get(&ino) {
            Some(f) => f.path.clone(),
            None => return Err(libc::ENOENT),
        };
        self.ensure_buffer(ino, &path);
        let content = {
            let mut buffers = self.write_buffers.lock();
            let buf = buffers.get_mut(&ino).expect("buffer guaranteed above");
            buf.set_len(new_size);
            self.maybe_spill(buf);
            self.snapshot_buffer_content(buf)
        };
        if let Some(f) = self.files_by_inode.get_mut(&ino) {
            f.size = new_size;
        }
        self.sync.queue_write_content(&path, content);
        // Buffer content was just queued — the inode is "clean" again; a
        // subsequent flush without new writes must not duplicate the op.
        self.dirty.remove(&ino);
        self.read_cache.remove(ino);
        Ok(())
    }

    /// `setattr`: mode override, truncate and times. Returns the attributes
    /// the kernel caches for the TTL.
    pub fn apply_setattr(
        &mut self,
        ino: u64,
        mode: Option<u32>,
        size: Option<u64>,
        _atime: Option<SystemTime>,
        mtime: Option<SystemTime>,
    ) -> Result<FileAttr, i32> {
        if let Some(m) = mode {
            self.mode_overrides
                .lock()
                .insert(ino, (m as u16) & 0o7777);
        }

        if let Some(new_size) = size {
            // Errors ignored on purpose: a size setattr on an unknown inode
            // keeps the previous behavior (falls through to the reply below).
            let _ = self.apply_truncate(ino, new_size);
        }

        // atime mirrors the mtime (single time slot), so an atime-only
        // setattr — Finder/Quick Look stamping "last opened" — is accepted
        // and dropped instead of passing for a modification.
        if let Some(t) = mtime {
            self.times.lock().insert(ino, t);
        } else if mode.is_some() || size.is_some() {
            self.touch(ino);
        }

        // The kernel caches this reply for the TTL: it must carry the same
        // size getattr would (the real one for a chunked file, not the
        // manifest's), or reads past the manifest size hit EOF meanwhile.
        self.apply_getattr(ino)
    }

    /// Reads a range of the file. Source order: write buffer (pending
    /// mutations), blob cache, blob at HEAD (a miss populates the cache).
    /// Git errors become EIO.
    pub fn apply_read(&mut self, ino: u64, offset: usize, size: usize) -> Result<Vec<u8>, i32> {
        fn slice_of(data: &[u8], offset: usize, size: usize) -> Vec<u8> {
            let start = offset.min(data.len());
            let end = (offset + size).min(data.len());
            data[start..end].to_vec()
        }

        if let Some(buf) = self.write_buffers.lock().get(&ino) {
            return Ok(buf.read_range(offset, size));
        }
        let (path, local_only, kind, file_size) = match self.files_by_inode.get(&ino) {
            Some(f) => (f.path.clone(), f.local_only, f.kind, f.size),
            None => return Err(libc::ENOENT),
        };
        // Chunked: reassembled per-chunk read (hash verification);
        // the BlobCache never stores the raw manifest nor chunks.
        if let Some(m) = self.manifest_for(ino) {
            return self.read_chunked(&m, offset, size);
        }
        // Large file: range read from the working tree. Loading the whole
        // blob per request (the old path, still the fallback) cost seconds
        // per MiB once the file no longer fit the blob cache.
        if kind == git::FileKind::Regular && file_size >= RANGE_READ_MIN_BYTES {
            if let Some(result) = self.read_worktree_range(ino, &path, local_only, offset, size) {
                return result;
            }
        }
        if let Some(data) = self.read_cache.get(ino) {
            return Ok(slice_of(data, offset, size));
        }
        let content = if local_only {
            self.read_local_only_content(kind, &path).ok_or(libc::EIO)?
        } else {
            self.head_blob_via_persistent_handle(&path)?
        };
        let out = slice_of(&content, offset, size);
        self.read_cache.insert(ino, content);
        Ok(out)
    }

    /// Range read of a large file straight from its working-tree copy.
    /// Committed: checked once against the blob id at HEAD (plus the AEAD tag
    /// when sealed). Local-only: read as is. `None` = use the object store
    /// instead (copy missing or not the committed version).
    fn read_worktree_range(
        &mut self,
        ino: u64,
        path: &str,
        local_only: bool,
        offset: usize,
        size: usize,
    ) -> Option<Result<Vec<u8>, i32>> {
        use crate::range_read::{Expect, RangeError};
        let (rel, expect) = if local_only {
            (path.to_string(), Expect::Unverified)
        } else {
            let disk = self.disk_path(path);
            let oid = self.head_blob_id(ino, &disk)?;
            (disk, Expect::GitBlob(oid))
        };
        let full = std::path::Path::new(&self.repo_path).join(&rel);
        match self.range.read(&full, &expect, self.crypto.as_deref(), None, offset as u64, size) {
            Ok((data, _)) => Some(Ok(data)),
            Err(RangeError::Corrupt) => {
                log::error!("range read: {} fails its integrity check — EIO", path);
                Some(Err(libc::EIO))
            }
            Err(RangeError::Mismatch) | Err(RangeError::Io) => {
                if local_only {
                    return Some(Err(libc::EIO));
                }
                if self.range_fallbacks.insert(ino) {
                    log::warn!(
                        "range read: working-tree copy of {} is not the committed version; \
                         reading from the object store (slow for large files)",
                        path
                    );
                }
                None
            }
        }
    }

    /// Blob id of `disk_path` at HEAD (tree lookup, no blob load), cached per
    /// inode until HEAD changes.
    fn head_blob_id(&mut self, ino: u64, disk_path: &str) -> Option<git2::Oid> {
        if let Some(oid) = self.head_blob_ids.get(&ino) {
            return Some(*oid);
        }
        if self.read_repo.is_none() {
            self.read_repo = git::open_repo(&self.repo_path).ok();
            if self.read_repo.is_some() {
                self.read_repo_opens += 1;
            }
        }
        let repo = self.read_repo.as_ref()?;
        let oid = git::get_commit(repo, "HEAD")
            .and_then(|c| c.tree())
            .and_then(|t| t.get_path(std::path::Path::new(disk_path)))
            .map(|e| e.id())
            .ok()?;
        self.head_blob_ids.insert(ino, oid);
        Some(oid)
    }

    /// Target of a symlink (blob content = target text). EINVAL for a
    /// regular file, ENOENT for an unknown inode.
    pub fn apply_readlink(&mut self, ino: u64) -> Result<Vec<u8>, i32> {
        match self.files_by_inode.get(&ino) {
            Some(f) if f.kind == git::FileKind::Symlink => {}
            Some(_) => return Err(libc::EINVAL),
            None => return Err(libc::ENOENT),
        }
        let raw = self.apply_read(ino, 0, usize::MAX)?;
        if let Some(c) = &self.crypto {
            // The on-disk target is encrypted per component; the view is clear.
            // Hashed (`dslong_`) components resolve through their worktree
            // sidecars (present whenever the target path exists on the drive);
            // a dangling target with an unknown hash stays literal.
            let s = String::from_utf8_lossy(&raw);
            return Ok(crate::sync::dec_disk_path_with_worktree_sidecars(
                c,
                std::path::Path::new(&self.repo_path),
                &s,
            )
            .into_bytes());
        }
        Ok(raw)
    }

    /// Path as it exists on DISK/HEAD: encrypted when encryption is active.
    /// Deterministic — long components map to their `dslong_<hash>` name, so
    /// lookups resolve without consulting sidecars.
    fn disk_path(&self, clear: &str) -> String {
        match &self.crypto {
            Some(c) => c.enc_path(clear),
            None => clear.to_string(),
        }
    }

    /// Decrypts sealed bytes when crypto is active; content without the magic
    /// passes through (mixed state). AEAD failure = EIO, never garbage.
    fn maybe_open(&self, bytes: Vec<u8>) -> Result<Vec<u8>, i32> {
        match &self.crypto {
            Some(c) if crate::crypt::Crypto::is_encrypted_bytes(&bytes) => {
                c.open(&bytes).map_err(|e| {
                    log::error!("crypto: {} — EIO", e);
                    libc::EIO
                })
            }
            _ => Ok(bytes),
        }
    }

    /// HEAD blob via persistent handle — opens the repo once and reuses it
    /// for subsequent reads. An error drops the handle (reopened on the next
    /// call) and becomes EIO. With encryption, looks up the encrypted path
    /// and decrypts.
    fn head_blob_via_persistent_handle(&mut self, path: &str) -> Result<Vec<u8>, i32> {
        if self.read_repo.is_none() {
            self.read_repo = git::open_repo(&self.repo_path).ok();
            if self.read_repo.is_some() {
                self.read_repo_opens += 1;
            }
        }
        let disk = self.disk_path(path);
        let Some(repo) = self.read_repo.as_ref() else {
            return Err(libc::EIO);
        };
        let result = git::get_commit(repo, "HEAD")
            .and_then(|commit| git::read_file_at_commit(repo, &commit, &disk));
        match result {
            Ok(content) => self.maybe_open(content),
            Err(_) => {
                // The handle may have become invalid (repo moved, external
                // gc): drop it so the next read reopens.
                self.read_repo = None;
                Err(libc::EIO)
            }
        }
    }

    /// Whole-file checks done by range reads (introspection): one per
    /// working-tree file served, not one per read.
    pub fn range_verifications_for_test(&self) -> u64 {
        self.range.verifications()
    }

    /// Actual opens of the persistent read handle (introspection).
    pub fn read_repo_opens_for_test(&self) -> u64 {
        self.read_repo_opens
    }

    /// Flush of an inode: queues the buffer into sync ONLY if there was a
    /// write since the last flush (dirty). Returns the queued path, or `None`
    /// if there was nothing to do (e.g. close of a read-only fd).
    pub fn apply_flush(&mut self, ino: u64) -> Option<String> {
        if !self.dirty.contains(&ino) {
            return None;
        }
        let path = self.files_by_inode.get(&ino)?.path.clone();
        let content = {
            let buffers = self.write_buffers.lock();
            let buf = buffers.get(&ino)?;
            self.snapshot_buffer_content(buf)
        };
        self.sync.queue_write_content(&path, content);
        self.dirty.remove(&ino);
        self.touch(ino);
        Some(path)
    }

    /// Discards buffers of inodes with no pending writes (non-dirty) and the
    /// entire blob cache — the call sites mean "HEAD changed".
    /// Dirty buffers are never touched — their content has not reached
    /// sync yet.
    fn gc_clean_buffers(&mut self) {
        let dirty = &self.dirty;
        self.write_buffers.lock().retain(|ino, _| dirty.contains(ino));
        self.read_cache.clear();
        // HEAD changed: manifests may have changed content/state.
        self.manifests.lock().clear();
        // Handles of files gone from the working tree (deleted, GC'd
        // chunks) would pin their disk space until evicted.
        self.range.prune_missing();
        self.head_blob_ids.clear();
        self.range_fallbacks.clear();
    }

    /// Current content visible for reading (buffer or blob at HEAD).
    pub fn read_content_for_test(&self, ino: u64) -> Option<Vec<u8>> {
        self.current_file_content(ino)
    }

    /// Number of live write buffers (test introspection).
    pub fn buffered_count_for_test(&self) -> usize {
        self.write_buffers.lock().len()
    }

    /// `Arc` of an inode's buffer (introspection for tests of content
    /// sharing with the queue).
    pub fn buffer_arc_for_test(&self, ino: u64) -> Option<Arc<Vec<u8>>> {
        match self.write_buffers.lock().get(&ino) {
            Some(WriteBuffer::Mem(a)) => Some(a.clone()),
            _ => None,
        }
    }

    /// `true` if the inode's buffer lives in a spool file (introspection).
    pub fn buffer_is_spooled_for_test(&self, ino: u64) -> bool {
        matches!(
            self.write_buffers.lock().get(&ino),
            Some(WriteBuffer::Spooled(_))
        )
    }

    pub fn read_cache_contains_for_test(&self, ino: u64) -> bool {
        self.read_cache.contains(ino)
    }

    pub fn read_cache_len_for_test(&self) -> usize {
        self.read_cache.len()
    }

    /// Accumulated (hits, misses) of the read cache.
    pub fn read_cache_stats_for_test(&self) -> (u64, u64) {
        self.read_cache.stats()
    }

    pub fn apply_create(
        &mut self,
        parent: u64,
        name: &str,
    ) -> Result<(u64, FileAttr), i32> {
        if !self.dirs_by_inode.contains_key(&parent) {
            if self.files_by_inode.contains_key(&parent) {
                return Err(libc::ENOTDIR);
            }
            return Err(libc::ENOENT);
        }
        if self.child_exists(parent, name) {
            return Err(libc::EEXIST);
        }

        let parent_path = self.dirs_by_inode.get(&parent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        let inode = self.alloc_inode();
        let entry = FileEntry {
            inode,
            path: full_path.clone(),
            parent,
            size: 0,
            kind: git::FileKind::Regular,
            local_only: false,
        };
        self.files_by_inode.insert(inode, entry);
        self.inodes_by_path.insert(full_path.clone(), inode);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(inode);

        // Queue an empty write to materialize on disk (a later flush may overwrite)
        self.sync.queue_write(&full_path, Vec::new());
        // Empty (non-dirty) buffer so reads before the commit return empty
        // instead of EIO — the new path does not exist in HEAD yet.
        self.write_buffers
            .lock()
            .insert(inode, WriteBuffer::Mem(Arc::new(Vec::new())));

        Ok((inode, self.file_attr_for(inode, 0)))
    }

    /// Creates a symlink `parent/name -> target`. The target is opaque text
    /// (never resolved by git-drive); the committed blob is the text itself.
    pub fn apply_symlink(
        &mut self,
        parent: u64,
        name: &str,
        target: &str,
    ) -> Result<(u64, FileAttr), i32> {
        if !self.dirs_by_inode.contains_key(&parent) {
            if self.files_by_inode.contains_key(&parent) {
                return Err(libc::ENOTDIR);
            }
            return Err(libc::ENOENT);
        }
        if self.child_exists(parent, name) {
            return Err(libc::EEXIST);
        }

        let parent_path = self.dirs_by_inode.get(&parent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        let inode = self.alloc_inode();
        let entry = FileEntry {
            inode,
            path: full_path.clone(),
            parent,
            size: target.len() as u64,
            kind: git::FileKind::Symlink,
            local_only: false,
        };
        self.files_by_inode.insert(inode, entry);
        self.inodes_by_path.insert(full_path.clone(), inode);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(inode);

        // (Non-dirty) buffer with the target text: readlink works before the
        // commit — the path does not exist in HEAD yet.
        self.write_buffers
            .lock()
            .insert(inode, WriteBuffer::Mem(Arc::new(target.as_bytes().to_vec())));
        self.sync.queue_symlink(&full_path, target);
        self.touch(inode);

        Ok((inode, self.file_attr_for(inode, target.len() as u64)))
    }

    pub fn apply_unlink(&mut self, parent: u64, name: &str) -> Result<(), i32> {
        let dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        let mut target_ino: Option<u64> = None;
        let mut is_dir = false;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    target_ino = Some(*child);
                    break;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    target_ino = Some(*child);
                    is_dir = true;
                    break;
                }
            }
        }
        let Some(ino) = target_ino else {
            return Err(libc::ENOENT);
        };
        if is_dir {
            return Err(libc::EISDIR);
        }
        let entry = self.files_by_inode.remove(&ino).unwrap();
        self.inodes_by_path.remove(&entry.path);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .remove(&ino);
        self.write_buffers.lock().remove(&ino);
        self.dirty.remove(&ino);
        self.read_cache.remove(ino);
        self.manifests.lock().remove(&ino);
        self.sync.queue_delete(&entry.path);
        Ok(())
    }

    fn child_exists(&self, parent: u64, name: &str) -> bool {
        let Some(dir) = self.dirs_by_inode.get(&parent) else {
            return false;
        };
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    return true;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    return true;
                }
            }
        }
        false
    }

    pub fn sync_snapshot_for_test(&self) -> Vec<crate::sync::SyncOp> {
        self.sync.buffer_snapshot()
    }

    pub fn sync_queue_write_for_test(&self, path: &str, content: Vec<u8>) {
        self.sync.queue_write(path, content);
    }

    /// If the `SyncManager` signaled changes coming from the pull watcher,
    /// rebuilds the `files_by_inode`/`dirs_by_inode`/`inodes_by_path` indexes.
    /// Preserves inodes of still-existing paths and never regresses `next_inode`.
    pub fn refresh_if_needed(&mut self) {
        // Critical guard: if there are pending ops in the sync buffer
        // (writes/mkdirs/etc not yet drained by the debounce watcher), do NOT
        // refresh — the refresh rebuilds indexes from git HEAD, discarding any
        // locally created file/dir that has not reached a commit yet. Without
        // this guard, paths just created via CREATE/MKDIR vanished from the
        // indexes, and the next rename/unlink/rmdir from Finder returned
        // ENOENT — Finder shows "(error code -43)". The flag stays set (we do
        // not consume it with take_refresh_flag), so the refresh runs on the
        // next lookup/getattr/readdir after the drain.
        if self.sync.has_pending_ops() {
            return;
        }
        // GC of clean buffers when a new local commit happened: with the sync
        // buffer empty (checked above), everything queued has been drained,
        // and the commit guarantees the content is in HEAD. Discarding the
        // buffer makes `read` see HEAD again — without this, remote changes
        // to that file would stay invisible forever.
        let gen = self.sync.commit_generation();
        if gen != self.last_commit_gen {
            self.last_commit_gen = gen;
            self.gc_clean_buffers();
            // Ignored files that took part in the batch stayed out of the
            // commit — marking them local-only redirects reads to the
            // working tree (the buffer that served them was just collected).
            self.incorporate_local_only(&HashMap::new());
        }
        if !self.sync.take_refresh_flag() {
            return;
        }
        // Post-pull refresh: HEAD advanced via fetch/merge (which only runs
        // with a clean working tree). Clean buffers reflect the old HEAD — discard.
        let t0 = std::time::Instant::now();
        self.gc_clean_buffers();
        let repo = match git::open_repo(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                log::error!("refresh_if_needed: error opening repo: {}", e);
                return;
            }
        };
        let commit = match git::get_commit(&repo, "HEAD") {
            Ok(c) => c,
            Err(e) => {
                log::error!("refresh_if_needed: error resolving HEAD: {}", e);
                return;
            }
        };
        let dirs = match git::list_dirs_at_commit(&repo, &commit) {
            Ok(d) => d,
            Err(e) => {
                log::error!("refresh_if_needed: list_dirs failed: {}", e);
                return;
            }
        };
        let files = match git::list_files_at_commit(&repo, &commit) {
            Ok(f) => f,
            Err(e) => {
                log::error!("refresh_if_needed: list_files failed: {}", e);
                return;
            }
        };
        let new_head = commit.id();
        let (dirs, files, name_cache) = decrypt_listing_cached(
            self.crypto.as_deref(),
            &self.repo_path,
            dirs,
            files,
            &self.name_cache,
        );
        self.name_cache = name_cache;

        let previous: HashMap<String, u64> = self.inodes_by_path.clone();

        let mut files_by_inode: HashMap<u64, FileEntry> = HashMap::new();
        let mut dirs_by_inode: HashMap<u64, DirEntry> = HashMap::new();
        let mut inodes_by_path: HashMap<String, u64> = HashMap::new();

        let root = DirEntry {
            inode: ROOT_INODE,
            path: String::new(),
            parent: 0,
            children: HashSet::new(),
        };
        dirs_by_inode.insert(ROOT_INODE, root);
        inodes_by_path.insert(String::new(), ROOT_INODE);

        let mut dirs_sorted = dirs;
        dirs_sorted.sort_by_key(|p| p.matches('/').count());

        for dir_path in dirs_sorted {
            if crate::reserved::is_reserved_path(&dir_path) {
                continue;
            }
            // `unwrap_or_else` (lazy): the eager form with `unwrap_or` always
            // evaluated the alloc, leaking one inode per existing path on
            // every refresh.
            let inode = previous
                .get(&dir_path)
                .copied()
                .unwrap_or_else(|| self.alloc_inode());
            let parent_path = Self::resolve_parent_path(&dir_path);
            let parent_inode = *inodes_by_path.get(&parent_path).unwrap_or(&ROOT_INODE);
            let entry = DirEntry {
                inode,
                path: dir_path.clone(),
                parent: parent_inode,
                children: HashSet::new(),
            };
            dirs_by_inode.insert(inode, entry);
            inodes_by_path.insert(dir_path, inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        for git::ListedFile { path: file_path, size, kind } in files {
            // Same rule as the initial ingestion: `.gitkeep` and `.chunks`
            // stay out of the indexes.
            if leaf_name(&file_path) == ".gitkeep" || crate::reserved::is_reserved_path(&file_path) {
                continue;
            }
            let inode = previous
                .get(&file_path)
                .copied()
                .unwrap_or_else(|| self.alloc_inode());
            let parent_path = Self::resolve_parent_path(&file_path);
            let parent_inode = *inodes_by_path.get(&parent_path).unwrap_or(&ROOT_INODE);
            let entry = FileEntry {
                inode,
                path: file_path.clone(),
                parent: parent_inode,
                size,
                kind,
                local_only: false,
            };
            files_by_inode.insert(inode, entry);
            inodes_by_path.insert(file_path, inode);
            dirs_by_inode
                .get_mut(&parent_inode)
                .unwrap()
                .children
                .insert(inode);
        }

        self.files_by_inode = files_by_inode;
        self.dirs_by_inode = dirs_by_inode;
        self.inodes_by_path = inodes_by_path;
        // Local-only entries do not live in HEAD: reincorporate them from the
        // working tree, reusing inodes from the previous index so kernel
        // handles stay valid.
        self.incorporate_local_only(&previous);
        // HEAD advanced: paths touched by the pull get the new commit's mtime.
        match self.indexed_head {
            Some(old) if old != new_head => self.refresh_derived_times_since(old, new_head),
            Some(_) => self.derived_times.retain(|ino, _| self.files_by_inode.contains_key(ino)),
            None => self.refresh_derived_times(),
        }
        self.indexed_head = Some(new_head);
        log::info!(
            "refresh: index rebuilt in {:.1?} ({} files)",
            t0.elapsed(),
            self.files_by_inode.len()
        );
        // The previous index and the tree listing are gone.
        crate::release_freed_memory();
    }

    pub fn apply_mkdir(&mut self, parent: u64, name: &str) -> Result<(u64, FileAttr), i32> {
        if !self.dirs_by_inode.contains_key(&parent) {
            if self.files_by_inode.contains_key(&parent) {
                return Err(libc::ENOTDIR);
            }
            return Err(libc::ENOENT);
        }
        if self.child_exists(parent, name) {
            return Err(libc::EEXIST);
        }
        let parent_path = self.dirs_by_inode.get(&parent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", parent_path, name)
        };

        let inode = self.alloc_inode();
        let entry = DirEntry {
            inode,
            path: full_path.clone(),
            parent,
            children: HashSet::new(),
        };
        self.dirs_by_inode.insert(inode, entry);
        self.inodes_by_path.insert(full_path.clone(), inode);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(inode);

        self.sync.queue_mkdir(&full_path);
        Ok((inode, self.dir_attr_for(inode)))
    }

    /// Content swap between the single file child of `source_parent` and the
    /// same-named file in `target_parent`. Used when macFUSE sends
    /// FUSE_RENAME with empty names as an alternate encoding of exchangedata.
    /// Semantically: after the call, both files still exist, but with their
    /// contents swapped.
    fn apply_implicit_swap(&mut self, source_parent: u64, target_parent: u64) -> Result<(), i32> {
        // When source_parent == target_parent, RENAME parent/"" -> parent/"" is
        // a post-commit signal from macFUSE (metadata refresh), not a real
        // TextEdit swap. Return Ok as a no-op before trying to pick a file —
        // without this, if the dir has no files (only dirs), picking fails and
        // we return ENOENT, which becomes -43 in Finder.
        if source_parent == target_parent {
            return Ok(());
        }
        let (src_ino, src_name, src_path) = {
            let dir = self.dirs_by_inode.get(&source_parent).ok_or(libc::ENOENT)?;
            // The macFUSE encoding does not carry WHICH file to swap — the
            // safe-save contract is a temp dir with ONE file. With zero or
            // more than one, any choice is a guess, and a wrong guess swaps
            // the content of the wrong file (silent corruption). EXDEV
            // degrades the app to the copy flow, which works.
            let file_children: Vec<u64> = dir
                .children
                .iter()
                .filter(|c| self.files_by_inode.contains_key(c))
                .copied()
                .collect();
            if file_children.len() != 1 {
                log::warn!(
                    "ambiguous implicit swap: source_parent={} has {} files (expected 1) — returning EXDEV",
                    source_parent,
                    file_children.len()
                );
                return Err(libc::EXDEV);
            }
            let ino = file_children[0];
            let entry = self.files_by_inode.get(&ino).unwrap();
            (ino, leaf_name(&entry.path).to_string(), entry.path.clone())
        };

        let (tgt_ino, tgt_path) = {
            let dir = self.dirs_by_inode.get(&target_parent).ok_or(libc::ENOENT)?;
            let mut picked: Option<u64> = None;
            for child in &dir.children {
                if let Some(f) = self.files_by_inode.get(child) {
                    if leaf_name(&f.path) == src_name {
                        picked = Some(*child);
                        break;
                    }
                }
            }
            // No counterpart in the target: not a swap (typical case =
            // Finder's "Move to Trash", where target=.Trashes/<uid> is
            // empty). EXDEV signals "cross-device operation" to the caller —
            // preferable to ENOENT (which becomes "-43 items can't be found"
            // in Finder). Ideally the MKDIR of .Trashes was already rejected
            // earlier; this is defense in depth for other ambiguous
            // exchangedata encodings.
            let Some(ino) = picked else {
                log::warn!(
                    "ambiguous implicit swap: src={:?} has no counterpart in target_parent={} — returning EXDEV",
                    src_name,
                    target_parent
                );
                return Err(libc::EXDEV);
            };
            let entry = self.files_by_inode.get(&ino).unwrap();
            (ino, entry.path.clone())
        };

        // Guard: if source and target are the same inode, there is nothing to
        // swap. Happens when Finder emits RENAME 1/"" -> 1/"" on metadata
        // updates — not a real save, return Ok silently.
        if src_ino == tgt_ino {
            return Ok(());
        }

        let src_content = self
            .content_for_inode_at_path(src_ino, &src_path)
            .unwrap_or_default();
        let tgt_content = self
            .content_for_inode_at_path(tgt_ino, &tgt_path)
            .unwrap_or_default();

        let src_new_size = tgt_content.len() as u64;
        let tgt_new_size = src_content.len() as u64;
        if let Some(f) = self.files_by_inode.get_mut(&src_ino) {
            f.size = src_new_size;
        }
        if let Some(f) = self.files_by_inode.get_mut(&tgt_ino) {
            f.size = tgt_new_size;
        }

        let tgt_shared = Arc::new(tgt_content);
        let src_shared = Arc::new(src_content);
        {
            let mut buffers = self.write_buffers.lock();
            buffers.insert(src_ino, WriteBuffer::Mem(tgt_shared.clone()));
            buffers.insert(tgt_ino, WriteBuffer::Mem(src_shared.clone()));
        }

        self.sync
            .queue_write_content(&src_path, crate::sync::WriteContent::Mem(tgt_shared));
        self.sync
            .queue_write_content(&tgt_path, crate::sync::WriteContent::Mem(src_shared));
        self.dirty.remove(&src_ino);
        self.dirty.remove(&tgt_ino);
        self.touch(src_ino);
        self.touch(tgt_ino);

        Ok(())
    }

    /// Atomic content exchange between two named files — the semantics of
    /// exchangedata(2)/renameatx_np(RENAME_SWAP). Used by the `exchange`
    /// callback (FUSE_EXCHANGE) and by `rename` with the SWAP flag.
    pub fn apply_exchange(
        &mut self,
        parent: u64,
        name: &str,
        newparent: u64,
        newname: &str,
    ) -> Result<(), i32> {
        let ino_a = match self.find_child_inode(parent, name) {
            Some(i) if self.files_by_inode.contains_key(&i) => i,
            Some(_) => return Err(libc::EISDIR),
            None => return Err(libc::ENOENT),
        };
        let ino_b = match self.find_child_inode(newparent, newname) {
            Some(i) if self.files_by_inode.contains_key(&i) => i,
            Some(_) => return Err(libc::EISDIR),
            None => return Err(libc::ENOENT),
        };

        let content_a = self.current_file_content(ino_a).unwrap_or_default();
        let content_b = self.current_file_content(ino_b).unwrap_or_default();

        let path_a = self.files_by_inode.get(&ino_a).map(|f| f.path.clone());
        let path_b = self.files_by_inode.get(&ino_b).map(|f| f.path.clone());
        let (Some(path_a), Some(path_b)) = (path_a, path_b) else {
            return Err(libc::ENOENT);
        };

        // Swap sizes in memory
        let size_a = content_a.len() as u64;
        let size_b = content_b.len() as u64;
        if let Some(f) = self.files_by_inode.get_mut(&ino_a) {
            f.size = size_b;
        }
        if let Some(f) = self.files_by_inode.get_mut(&ino_b) {
            f.size = size_a;
        }

        // Buffers reflect the swapped content for reads before the commit
        let shared_b = Arc::new(content_b);
        let shared_a = Arc::new(content_a);
        {
            let mut buffers = self.write_buffers.lock();
            buffers.insert(ino_a, WriteBuffer::Mem(shared_b.clone()));
            buffers.insert(ino_b, WriteBuffer::Mem(shared_a.clone()));
        }

        self.sync
            .queue_write_content(&path_a, crate::sync::WriteContent::Mem(shared_b));
        self.sync
            .queue_write_content(&path_b, crate::sync::WriteContent::Mem(shared_a));
        self.dirty.remove(&ino_a);
        self.dirty.remove(&ino_b);
        self.read_cache.remove(ino_a);
        self.read_cache.remove(ino_b);

        self.touch(ino_a);
        self.touch(ino_b);
        Ok(())
    }

    pub fn apply_rename(
        &mut self,
        parent: u64,
        name: &str,
        new_parent: u64,
        new_name: &str,
    ) -> Result<(), i32> {
        // Defensive fallback: empty names were an artifact of the misaligned
        // fuse_rename_in parse in macFUSE (8 bytes of flags/padding read as
        // names) — fixed in vendor/fuser. If some encoding still arrives like
        // this, fall back to the old single-file-child swap heuristic.
        if name.is_empty() && new_name.is_empty() {
            return self.apply_implicit_swap(parent, new_parent);
        }

        // 1. Resolve the source in `parent`
        let src_dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        let mut src_ino: Option<u64> = None;
        let mut src_is_dir = false;
        for child in &src_dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    src_ino = Some(*child);
                    break;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    src_ino = Some(*child);
                    src_is_dir = true;
                    break;
                }
            }
        }
        let Some(src) = src_ino else {
            return Err(libc::ENOENT);
        };

        // 2. Validate new_parent is a directory
        if !self.dirs_by_inode.contains_key(&new_parent) {
            return Err(libc::ENOTDIR);
        }

        // 3. Check for an existing destination
        let mut dest_existing: Option<(u64, bool)> = None;
        {
            let dst_dir = self.dirs_by_inode.get(&new_parent).unwrap();
            for child in &dst_dir.children {
                if let Some(f) = self.files_by_inode.get(child) {
                    if leaf_name(&f.path) == new_name {
                        dest_existing = Some((*child, false));
                        break;
                    }
                } else if let Some(d) = self.dirs_by_inode.get(child) {
                    if leaf_name(&d.path) == new_name {
                        dest_existing = Some((*child, true));
                        break;
                    }
                }
            }
        }
        if let Some((_dest_ino, dest_is_dir)) = dest_existing {
            if src_is_dir || dest_is_dir {
                return Err(libc::EEXIST);
            }
            // File over file: overwrite (implicit unlink of the destination)
            self.apply_unlink(new_parent, new_name)?;
        }

        // 4. Build the new path
        let new_parent_path = self.dirs_by_inode.get(&new_parent).unwrap().path.clone();
        let new_path = if new_parent_path.is_empty() {
            new_name.to_string()
        } else {
            format!("{}/{}", new_parent_path, new_name)
        };

        // 5. Capture the old path
        let old_path = if src_is_dir {
            self.dirs_by_inode.get(&src).unwrap().path.clone()
        } else {
            self.files_by_inode.get(&src).unwrap().path.clone()
        };

        // 6. Remove from the old parent, move to new_parent
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .remove(&src);
        self.dirs_by_inode
            .get_mut(&new_parent)
            .unwrap()
            .children
            .insert(src);

        // 7. Update path/parent of the source entry and its descendants
        if src_is_dir {
            if let Some(d) = self.dirs_by_inode.get_mut(&src) {
                d.path = new_path.clone();
                d.parent = new_parent;
            }
            self.update_descendant_paths(&old_path, &new_path);
        } else {
            if let Some(f) = self.files_by_inode.get_mut(&src) {
                f.path = new_path.clone();
                f.parent = new_parent;
            }
        }
        // Re-index both paths (src and descendants were handled above)
        self.reindex_paths();

        if src_is_dir {
            let src_eph = crate::sync::is_fully_ephemeral_path(&old_path);
            let dest_eph = crate::sync::is_fully_ephemeral_path(&new_path);
            if dest_eph && !src_eph {
                // Moving a real dir into an ephemeral area (e.g. Trash) =
                // removal from the repo. The filter would discard the whole
                // Rename and the disk would diverge from the index. We expand
                // it into Delete/Rmdir of the OLD (real) paths, bottom-up.
                // Buffers keep the content in memory: reads inside the Trash
                // keep working and a rename back rematerializes.
                let (mut dirs, files) = self.collect_subtree(src);
                let to_old = |p: &str| format!("{}{}", old_path, &p[new_path.len()..]);
                for (ino, path) in &files {
                    let old = to_old(path);
                    let content = self
                        .content_for_inode_at_path(*ino, &old)
                        .unwrap_or_default();
                    self.write_buffers
                        .lock()
                        .insert(*ino, WriteBuffer::Mem(Arc::new(content)));
                    self.read_cache.remove(*ino);
                    self.dirty.remove(ino);
                    self.sync.queue_delete(&old);
                }
                dirs.sort_by_key(|(_, p)| std::cmp::Reverse(p.matches('/').count()));
                for (_, path) in &dirs {
                    self.sync.queue_rmdir(&to_old(path));
                }
            } else if src_eph && !dest_eph {
                // Ephemeral dir becoming real = materialization: the content
                // only exists in memory (or in HEAD, if it was committed
                // legacy). Mkdir top-down + Write with current content.
                let (mut dirs, files) = self.collect_subtree(src);
                let to_old = |p: &str| format!("{}{}", old_path, &p[new_path.len()..]);
                dirs.sort_by_key(|(_, p)| p.matches('/').count());
                for (_, path) in &dirs {
                    self.sync.queue_mkdir(path);
                }
                for (ino, path) in &files {
                    let old = to_old(path);
                    let content = self
                        .content_for_inode_at_path(*ino, &old)
                        .unwrap_or_default();
                    self.sync.queue_write(path, content);
                    self.dirty.remove(ino);
                }
            } else {
                // Directory: we use a native Rename on disk (mv). Empty/full
                // dirs transition just fine. (Ephemeral -> ephemeral lands
                // here and the enqueue filter discards it — memory only.)
                self.sync.queue_rename(&old_path, &new_path);
            }
        } else if self.manifest_for(src).is_some()
            && !self.write_buffers.lock().contains_key(&src)
        {
            // Chunked with no local edits: move only the MANIFEST — chunks
            // are content-addressed and stay where they are (the GC preserves
            // them while any manifest references them).
            let m = self.manifest_for(src).expect("checked above");
            self.read_cache.remove(src);
            self.sync.queue_write(&new_path, m.serialize());
            self.sync.queue_delete(&old_path);
            self.dirty.remove(&src);
        } else {
            // File: translates the rename into Write(dest) + Delete(src). This
            // decouples it from the src existing on disk (it may have been
            // created by ops coalesced in the buffer, never becoming a real
            // file). The content must be read via the OLD path, since the
            // in-memory path has already changed.
            let content = self
                .content_for_inode_at_path(src, &old_path)
                .unwrap_or_default();
            // A (non-dirty) buffer keeps the content readable until the
            // commit: the NEW path does not exist in HEAD yet — without this,
            // a post-rename read hit the nonexistent blob and returned EIO.
            // The gc discards it after the commit. The old path's cache entry
            // goes stale — out.
            let shared = Arc::new(content);
            self.write_buffers
                .lock()
                .insert(src, WriteBuffer::Mem(shared.clone()));
            self.read_cache.remove(src);
            self.sync
                .queue_write_content(&new_path, crate::sync::WriteContent::Mem(shared));
            self.sync.queue_delete(&old_path);
            // Current content (including pending writes) was queued at the
            // destination — the inode is "clean" again.
            self.dirty.remove(&src);
        }
        Ok(())
    }

    /// Incorporates the working tree's ignored files into the index (visible
    /// local-only entries). Paths already present are marked/updated; absent
    /// ones get a new entry (reusing the inode from `previous` when the path
    /// existed before a rebuild). Intermediate directories outside HEAD are
    /// created.
    fn incorporate_local_only(&mut self, previous: &HashMap<String, u64>) {
        // Encrypted mode: the repo's ignore semantics are disabled (rules
        // would match against encrypted names) — no local-only entries.
        if self.crypto.is_some() {
            return;
        }
        let repo = match git::open_repo(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                log::error!("local-only: error opening repo: {}", e);
                return;
            }
        };
        let ignored = match git::list_ignored_files(&repo) {
            Ok(v) => v,
            Err(e) => {
                log::error!("local-only: error listing ignored files: {}", e);
                return;
            }
        };
        for git::ListedFile { path, size, kind } in ignored {
            if leaf_name(&path) == ".gitkeep" || crate::sync::is_fully_ephemeral_path(&path) {
                continue;
            }
            if let Some(&ino) = self.inodes_by_path.get(&path) {
                if let Some(f) = self.files_by_inode.get_mut(&ino) {
                    f.local_only = true;
                    f.kind = kind;
                    // A pending buffer takes precedence over the on-disk size.
                    if !self.write_buffers.lock().contains_key(&ino) {
                        f.size = size;
                    }
                }
                continue;
            }
            let parent_path = Self::resolve_parent_path(&path);
            let parent_ino = self.ensure_dir_chain(&parent_path, previous);
            let ino = previous
                .get(&path)
                .copied()
                .unwrap_or_else(|| self.alloc_inode());
            let entry = FileEntry {
                inode: ino,
                path: path.clone(),
                parent: parent_ino,
                size,
                kind,
                local_only: true,
            };
            self.files_by_inode.insert(ino, entry);
            self.inodes_by_path.insert(path, ino);
            self.dirs_by_inode
                .get_mut(&parent_ino)
                .unwrap()
                .children
                .insert(ino);
        }
    }

    /// Ensures the directory chain of `dir_path` exists in the index,
    /// creating in-memory entries for dirs outside HEAD (e.g. an entirely
    /// ignored dir). Returns the inode of the final dir.
    fn ensure_dir_chain(&mut self, dir_path: &str, previous: &HashMap<String, u64>) -> u64 {
        if dir_path.is_empty() {
            return ROOT_INODE;
        }
        if let Some(&ino) = self.inodes_by_path.get(dir_path) {
            return ino;
        }
        let parent = self.ensure_dir_chain(&Self::resolve_parent_path(dir_path), previous);
        let ino = previous
            .get(dir_path)
            .copied()
            .unwrap_or_else(|| self.alloc_inode());
        let entry = DirEntry {
            inode: ino,
            path: dir_path.to_string(),
            parent,
            children: HashSet::new(),
        };
        self.dirs_by_inode.insert(ino, entry);
        self.inodes_by_path.insert(dir_path.to_string(), ino);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .insert(ino);
        ino
    }

    /// Cap on commits traversed by the mtime revwalk. Longer histories fall
    /// back (to the HEAD timestamp) so boot stays cheap.
    const MTIME_WALK_DEPTH_CAP: usize = 4096;

    /// Recomputes the derived mtime map (inode -> timestamp of the last
    /// commit that touched the path). Called at boot and on post-pull refresh.
    fn refresh_derived_times(&mut self) {
        let repo = match git::open_repo(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                log::error!("mtime: error opening repo: {}", e);
                return;
            }
        };
        let by_path = match git::commit_times_by_path(&repo, Self::MTIME_WALK_DEPTH_CAP) {
            Ok(m) => m,
            Err(e) => {
                log::error!("mtime: revwalk failed: {}", e);
                return;
            }
        };
        self.derived_times.clear();
        for (path, time) in by_path {
            if let Some(ino) = self.inode_of_disk_path(&path) {
                self.derived_times.insert(ino, time);
            }
        }
    }

    /// Clear-view inode of an on-disk (possibly encrypted) path. Goes through
    /// the name cache, which resolves `dslong_` names via their sidecars — the
    /// stateless `dec_path` left every long name literal, so in encrypted
    /// mode most files never got their mtime from history.
    fn inode_of_disk_path(&self, disk: &str) -> Option<u64> {
        let clear = match &self.crypto {
            Some(c) => match self.name_cache.get(disk) {
                Some(clear) => clear.clone(),
                None => c.dec_path(disk),
            },
            None => disk.to_string(),
        };
        self.inodes_by_path.get(&clear).copied()
    }

    /// Post-pull mtime update: only the paths touched by the commits between
    /// the previously indexed HEAD and the new one change; everything else
    /// keeps its time. Falls back to the full walk when the range is unusual
    /// (old HEAD unreachable, or longer than the walk cap).
    fn refresh_derived_times_since(&mut self, old: git2::Oid, new: git2::Oid) {
        let repo = match git::open_repo(&self.repo_path) {
            Ok(r) => r,
            Err(e) => {
                log::error!("mtime: error opening repo: {}", e);
                return;
            }
        };
        match git::commit_times_between(&repo, old, new, Self::MTIME_WALK_DEPTH_CAP) {
            Ok(Some(changed)) => {
                let files = &self.files_by_inode;
                self.derived_times.retain(|ino, _| files.contains_key(ino));
                for (path, time) in changed {
                    if let Some(ino) = self.inode_of_disk_path(&path) {
                        self.derived_times.insert(ino, time);
                    }
                }
            }
            Ok(None) => self.refresh_derived_times(),
            Err(e) => {
                log::warn!("mtime: incremental walk failed ({}), full recompute", e);
                self.refresh_derived_times();
            }
        }
    }

    /// Content of a local-only file, read from the working tree. For a
    /// symlink, the "content" is the target text (parity with Git's 120000
    /// blob).
    fn read_local_only_content(&self, kind: git::FileKind, path: &str) -> Option<Vec<u8>> {
        let full = std::path::Path::new(&self.repo_path).join(path);
        match kind {
            git::FileKind::Symlink => std::fs::read_link(&full)
                .ok()
                .map(|t| t.as_os_str().as_encoded_bytes().to_vec()),
            git::FileKind::Regular => std::fs::read(&full).ok(),
        }
    }

    /// Collects the subtree of `root_ino` (inclusive): dirs and files with
    /// their current paths from the index. Local traversal, no I/O.
    fn collect_subtree(&self, root_ino: u64) -> (SubtreeEntries, SubtreeEntries) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut stack = vec![root_ino];
        while let Some(ino) = stack.pop() {
            if let Some(d) = self.dirs_by_inode.get(&ino) {
                dirs.push((ino, d.path.clone()));
                stack.extend(d.children.iter().copied());
            } else if let Some(f) = self.files_by_inode.get(&ino) {
                files.push((ino, f.path.clone()));
            }
        }
        (dirs, files)
    }

    fn update_descendant_paths(&mut self, old_prefix: &str, new_prefix: &str) {
        let old_with_sep = format!("{}/", old_prefix);
        let new_with_sep = format!("{}/", new_prefix);
        for entry in self.dirs_by_inode.values_mut() {
            if entry.path.starts_with(&old_with_sep) {
                entry.path = format!("{}{}", new_with_sep, &entry.path[old_with_sep.len()..]);
            }
        }
        for entry in self.files_by_inode.values_mut() {
            if entry.path.starts_with(&old_with_sep) {
                entry.path = format!("{}{}", new_with_sep, &entry.path[old_with_sep.len()..]);
            }
        }
    }

    fn reindex_paths(&mut self) {
        self.inodes_by_path.clear();
        self.inodes_by_path.insert(String::new(), ROOT_INODE);
        for (ino, d) in &self.dirs_by_inode {
            if *ino != ROOT_INODE {
                self.inodes_by_path.insert(d.path.clone(), *ino);
            }
        }
        for (ino, f) in &self.files_by_inode {
            self.inodes_by_path.insert(f.path.clone(), *ino);
        }
    }

    pub fn apply_rmdir(&mut self, parent: u64, name: &str) -> Result<(), i32> {
        let dir = self.dirs_by_inode.get(&parent).ok_or(libc::ENOENT)?;
        let mut target_ino: Option<u64> = None;
        let mut is_file = false;
        for child in &dir.children {
            if let Some(f) = self.files_by_inode.get(child) {
                if leaf_name(&f.path) == name {
                    target_ino = Some(*child);
                    is_file = true;
                    break;
                }
            } else if let Some(d) = self.dirs_by_inode.get(child) {
                if leaf_name(&d.path) == name {
                    target_ino = Some(*child);
                    break;
                }
            }
        }
        let Some(ino) = target_ino else {
            return Err(libc::ENOENT);
        };
        if is_file {
            return Err(libc::ENOTDIR);
        }
        let target = self.dirs_by_inode.get(&ino).unwrap();
        if !target.children.is_empty() {
            return Err(libc::ENOTEMPTY);
        }
        let path = target.path.clone();
        self.dirs_by_inode.remove(&ino);
        self.inodes_by_path.remove(&path);
        self.dirs_by_inode
            .get_mut(&parent)
            .unwrap()
            .children
            .remove(&ino);
        self.sync.queue_rmdir(&path);
        Ok(())
    }

    pub fn apply_readdir(&self, ino: u64) -> Result<Vec<(u64, FileType, String)>, i32> {
        let dir = self.dirs_by_inode.get(&ino).ok_or(libc::ENOENT)?;
        let mut out: Vec<(u64, FileType, String)> = Vec::new();
        out.push((ino, FileType::Directory, ".".to_string()));
        out.push((
            dir.parent.max(ROOT_INODE),
            FileType::Directory,
            "..".to_string(),
        ));
        // Stable ordering by name. Without it, pagination across readdir
        // calls can shuffle entries (HashSet has non-deterministic order in
        // separate iterations after mutations).
        let mut children: Vec<(u64, FileType, String)> = dir
            .children
            .iter()
            .filter_map(|child| {
                if let Some(f) = self.files_by_inode.get(child) {
                    let ftype = match f.kind {
                        git::FileKind::Symlink => FileType::Symlink,
                        git::FileKind::Regular => FileType::RegularFile,
                    };
                    Some((*child, ftype, leaf_name(&f.path).to_string()))
                } else {
                    self.dirs_by_inode
                        .get(child)
                        .map(|d| (*child, FileType::Directory, leaf_name(&d.path).to_string()))
                }
            })
            .collect();
        children.sort_by(|a, b| a.2.cmp(&b.2));
        out.extend(children);
        Ok(out)
    }
}

impl Filesystem for GitDriveFS {
    fn init(
        &mut self,
        _req: &Request<'_>,
        config: &mut fuser::KernelConfig,
    ) -> Result<(), libc::c_int> {
        // On macOS, the FUSE_EXCHANGE_DATA capability must be declared during
        // the handshake so the macFUSE kernel forwards exchangedata(2) as
        // FUSE_EXCHANGE. Without this bit, TextEdit and iWork apps fail the
        // safe save with "The file doesn't exist" because the atomic swap
        // never reaches us.
        #[cfg(target_os = "macos")]
        {
            let _ = config.add_capabilities(fuser::consts::FUSE_EXCHANGE_DATA);
        }
        // `config` is only consulted on macOS (above); silence the unused
        // binding on other targets without renaming the parameter.
        #[cfg(not(target_os = "macos"))]
        let _ = &config;
        log::info!("boot: init ok");
        Ok(())
    }

    fn open(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        log::trace!("OPEN inode={}", ino);
        reply.opened(ino, 0);
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        match self.apply_write(ino, offset as usize, data) {
            Ok(written) => {
                log::trace!(
                    "WRITE inode={} offset={} bytes={}",
                    ino,
                    offset,
                    data.len()
                );
                reply.written(written);
            }
            Err(code) => reply.error(code),
        }
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: fuser::ReplyEmpty,
    ) {
        // The buffer is NOT drained here: the content stays available for
        // subsequent reads (and for `apply_implicit_swap`, which runs AFTER
        // the flush in TextEdit's safe save cycle). Queueing is gated on the
        // dirty flag in `apply_flush` — a read-only fd flush is a no-op.
        if let Some(path) = self.apply_flush(ino) {
            log::trace!("FLUSH inode={} path={}", ino, path);
        }
        reply.ok();
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        log::trace!("RELEASE inode={}", ino);
        reply.ok();
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        self.refresh_if_needed();
        match self.apply_getattr(ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(code) => reply.error(code),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        atime: Option<fuser::TimeOrNow>,
        mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        log::trace!(
            "SETATTR inode={} mode={:?} size={:?} mtime={:?}",
            ino,
            mode,
            size,
            mtime
        );
        let resolve = |t: fuser::TimeOrNow| match t {
            fuser::TimeOrNow::Now => SystemTime::now(),
            fuser::TimeOrNow::SpecificTime(st) => st,
        };
        match self.apply_setattr(ino, mode, size, atime.map(resolve), mtime.map(resolve)) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(code) => reply.error(code),
        }
    }

    /// `link(2)` — creates a hardlink. TextEdit and iWork apps use it in the
    /// "safe save" to keep a backup of the original file before overwriting.
    /// Without it the save fails (even with exchange implemented).
    ///
    /// Our internal model is 1 inode = 1 path, so a "real" hardlink is not
    /// possible. We simulate it via a COPY with a new inode + same content.
    /// This works for the safe save pattern because the backup is transient
    /// (deleted right after). If an app keeps the backup, it shows up as a
    /// normal file.
    fn link(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let Some(newname_str) = newname.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        log::debug!(
            "LINK ino={} -> parent={} name={:?}",
            ino,
            newparent,
            newname_str
        );

        if !self.files_by_inode.contains_key(&ino) {
            reply.error(libc::ENOENT);
            return;
        }
        if !self.dirs_by_inode.contains_key(&newparent) {
            reply.error(libc::ENOENT);
            return;
        }
        if self.child_exists(newparent, newname_str) {
            reply.error(libc::EEXIST);
            return;
        }

        let content = self.current_file_content(ino).unwrap_or_default();
        let size = content.len() as u64;

        let parent_path = self.dirs_by_inode.get(&newparent).unwrap().path.clone();
        let full_path = if parent_path.is_empty() {
            newname_str.to_string()
        } else {
            format!("{}/{}", parent_path, newname_str)
        };

        let src_kind = self
            .files_by_inode
            .get(&ino)
            .map(|f| f.kind)
            .unwrap_or(git::FileKind::Regular);
        let new_inode = self.alloc_inode();
        let entry = FileEntry {
            inode: new_inode,
            path: full_path.clone(),
            parent: newparent,
            size,
            kind: src_kind,
            local_only: false,
        };
        self.files_by_inode.insert(new_inode, entry);
        self.inodes_by_path.insert(full_path.clone(), new_inode);
        self.dirs_by_inode
            .get_mut(&newparent)
            .unwrap()
            .children
            .insert(new_inode);

        // Populate the write_buffer so immediate reads see the content
        let shared = Arc::new(content);
        self.write_buffers
            .lock()
            .insert(new_inode, WriteBuffer::Mem(shared.clone()));
        self.sync
            .queue_write_content(&full_path, crate::sync::WriteContent::Mem(shared));
        self.touch(new_inode);

        let attr = self.file_attr_for(new_inode, size);
        reply.entry(&TTL, &attr, 0);
    }

    /// `exchangedata()` on macOS — swaps the content of two files atomically.
    /// TextEdit and iWork apps use this in the "safe save". If we return
    /// ENOSYS (the default), the save fails with "The file doesn't exist"
    /// even after writing the new content to a temp dir.
    #[cfg(target_os = "macos")]
    fn exchange(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _options: u64,
        reply: ReplyEmpty,
    ) {
        let (Some(name), Some(newname)) = (name.to_str(), newname.to_str()) else {
            reply.error(libc::EINVAL);
            return;
        };
        log::debug!(
            "EXCHANGE {}/{} <-> {}/{}",
            parent,
            name,
            newparent,
            newname
        );

        match self.apply_exchange(parent, name, newparent, newname) {
            Ok(()) => reply.ok(),
            Err(code) => reply.error(code),
        }
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        const BSIZE: u32 = 4096;
        const NAMELEN: u32 = 255;

        let total_bytes: u64 = self.files_by_inode.values().map(|f| f.size).sum();
        let used_blocks = total_bytes.div_ceil(BSIZE as u64);

        // Volume advertised with ~1 TiB "free" so macOS apps do not treat the
        // mount as unusable ("Zero bytes available" made Finder fall into a
        // summary/categorization mode instead of listing files). The real
        // limit comes from the disk hosting the Git repo — this is a signal
        // to userland, not a quota.
        let total_blocks: u64 = 256 * 1024 * 1024;
        let free_blocks = total_blocks.saturating_sub(used_blocks);

        let files_used = (self.files_by_inode.len() + self.dirs_by_inode.len()) as u64;
        let files_total: u64 = 1_000_000;
        let ffree = files_total.saturating_sub(files_used);

        reply.statfs(
            total_blocks,
            free_blocks,
            free_blocks,
            files_total,
            ffree,
            BSIZE,
            NAMELEN,
            BSIZE,
        );
    }

    fn setxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        value: &[u8],
        flags: i32,
        _position: u32,
        reply: ReplyEmpty,
    ) {
        const XATTR_CREATE: i32 = 0x1;
        const XATTR_REPLACE: i32 = 0x2;

        let mut xattrs = self.xattrs.lock();
        let entry = xattrs.entry(ino).or_default();
        let exists = entry.contains_key(name);
        if flags & XATTR_CREATE != 0 && exists {
            reply.error(libc::EEXIST);
            return;
        }
        if flags & XATTR_REPLACE != 0 && !exists {
            // macOS uses ENOATTR (93) for "xattr does not exist"; ENODATA (96
            // on macOS, 61 on Linux) means something different and confuses
            // the kernel.
            reply.error(enoattr());
            return;
        }
        entry.insert(name.to_os_string(), value.to_vec());
        reply.ok();
    }

    fn getxattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        name: &OsStr,
        size: u32,
        reply: ReplyXattr,
    ) {
        let xattrs = self.xattrs.lock();
        let value = xattrs.get(&ino).and_then(|m| m.get(name));
        match value {
            Some(v) => {
                if size == 0 {
                    reply.size(v.len() as u32);
                } else if (size as usize) < v.len() {
                    reply.error(libc::ERANGE);
                } else {
                    reply.data(v);
                }
            }
            None => reply.error(enoattr()),
        }
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        let xattrs = self.xattrs.lock();
        let mut buf: Vec<u8> = Vec::new();
        if let Some(m) = xattrs.get(&ino) {
            for key in m.keys() {
                buf.extend_from_slice(key.as_encoded_bytes());
                buf.push(0);
            }
        }
        if size == 0 {
            reply.size(buf.len() as u32);
        } else if (size as usize) < buf.len() {
            reply.error(libc::ERANGE);
        } else {
            reply.data(&buf);
        }
    }

    fn removexattr(&mut self, _req: &Request<'_>, ino: u64, name: &OsStr, reply: ReplyEmpty) {
        let mut xattrs = self.xattrs.lock();
        let removed = xattrs
            .get_mut(&ino)
            .and_then(|m| m.remove(name))
            .is_some();
        if removed {
            reply.ok();
        } else {
            reply.error(enoattr());
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        // Only refresh on the first page; continuation calls (offset > 0) must
        // see the same snapshot as the first, or skipping entries shuffles the
        // iterator.
        if offset == 0 {
            self.refresh_if_needed();
        }
        match self.apply_readdir(ino) {
            Ok(entries) => {
                let start = offset as usize;
                for (i, (child_ino, kind, name)) in entries.iter().enumerate().skip(start) {
                    // fuser::ReplyDirectory::add returns true when the buffer
                    // is full — the entry was NOT accepted. We stop and let
                    // the kernel call again with offset = i (the next
                    // undelivered one).
                    let full = reply.add(*child_ino, (i + 1) as i64, *kind, name);
                    if full {
                        break;
                    }
                }
                reply.ok();
            }
            Err(code) => reply.error(code),
        }
    }

    fn readdirplus(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectoryPlus,
    ) {
        // macOS Finder uses `getattrlistbulk`, which macFUSE translates into
        // readdirplus. Without this implementation, the macFUSE fallback
        // returned an inconsistent listing (only directories showed up in
        // Finder).
        if offset == 0 {
            self.refresh_if_needed();
        }
        match self.apply_readdir(ino) {
            Ok(entries) => {
                let start = offset as usize;
                for (i, (child_ino, kind, name)) in entries.iter().enumerate().skip(start) {
                    let attr = match kind {
                        FileType::Directory => self.dir_attr_for(*child_ino),
                        // RegularFile and Symlink: the correct kind comes from
                        // the index inside `file_attr_for`.
                        _ => {
                            let size = self
                                .files_by_inode
                                .get(child_ino)
                                .map(|f| self.effective_size(f.inode, f.size))
                                .unwrap_or(0);
                            self.file_attr_for(*child_ino, size)
                        }
                    };
                    let full = reply.add(
                        *child_ino,
                        (i + 1) as i64,
                        name.as_str(),
                        &TTL,
                        &attr,
                        0,
                    );
                    if full {
                        break;
                    }
                }
                reply.ok();
            }
            Err(code) => reply.error(code),
        }
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        log::debug!(
            "CREATE parent={} name={:?}",
            parent,
            filename
        );
        match self.apply_create(parent, filename) {
            Ok((ino, attr)) => reply.created(&TTL, &attr, 0, ino, 0),
            Err(code) => reply.error(code),
        }
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        log::debug!(
            "UNLINK parent={} name={:?}",
            parent,
            filename
        );
        match self.apply_unlink(parent, filename) {
            Ok(()) => {
                log::debug!("UNLINK ok parent={} name={:?}", parent, filename);
                reply.ok()
            }
            Err(code) => {
                log::error!(
                    "UNLINK ERR parent={} name={:?} code={}",
                    parent,
                    filename,
                    code
                );
                reply.error(code)
            }
        }
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        // `.Trashes` is allowed (Finder's native Trash, Phase 2). The EPERM
        // that used to live here was a mitigation for macFUSE's broken rename
        // parse (empty names + ambiguous swap), fixed in b8f2647. The dir is
        // ephemeral: it exists only in memory, and moving items into it
        // becomes a removal from the repo (Phase 1 expansion) with content
        // recoverable via buffers until the Trash is emptied or remounted.
        log::debug!(
            "MKDIR parent={} name={:?}",
            parent,
            filename
        );
        match self.apply_mkdir(parent, filename) {
            Ok((_, attr)) => reply.entry(&TTL, &attr, 0),
            Err(code) => reply.error(code),
        }
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let Some(filename) = name.to_str() else {
            reply.error(libc::EINVAL);
            return;
        };
        log::debug!(
            "RMDIR parent={} name={:?}",
            parent,
            filename
        );
        match self.apply_rmdir(parent, filename) {
            Ok(()) => {
                log::debug!("RMDIR ok parent={} name={:?}", parent, filename);
                reply.ok()
            }
            Err(code) => {
                log::error!(
                    "RMDIR ERR parent={} name={:?} code={}",
                    parent,
                    filename,
                    code
                );
                reply.error(code)
            }
        }
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let (Some(n), Some(nn)) = (name.to_str(), new_name.to_str()) else {
            reply.error(libc::EINVAL);
            return;
        };
        log::debug!(
            "RENAME {}/{:?} -> {}/{:?} flags=0x{:x}",
            parent,
            n,
            new_parent,
            nn,
            flags
        );
        // renameatx_np(RENAME_SWAP): TextEdit/iWork use it in the safe save
        // when the FS advertises support. exchangedata semantics — content swap.
        if flags & APPLE_RENAME_SWAP != 0 {
            match self.apply_exchange(parent, n, new_parent, nn) {
                Ok(()) => {
                    log::debug!("RENAME(SWAP) ok {:?} <-> {:?}", n, nn);
                    reply.ok();
                }
                Err(code) => {
                    log::error!(
                        "RENAME(SWAP) ERR {:?} <-> {:?} code={}",
                        n,
                        nn,
                        code
                    );
                    reply.error(code);
                }
            }
            return;
        }
        // renameatx_np(RENAME_EXCL): fails if the destination already exists.
        if flags & APPLE_RENAME_EXCL != 0 && self.child_exists(new_parent, nn) {
            reply.error(libc::EEXIST);
            return;
        }
        match self.apply_rename(parent, n, new_parent, nn) {
            Ok(()) => {
                log::debug!(
                    "RENAME ok {}/{:?} -> {}/{:?}",
                    parent,
                    n,
                    new_parent,
                    nn
                );
                reply.ok()
            }
            Err(code) => {
                log::error!(
                    "RENAME ERR {}/{:?} -> {}/{:?} code={}",
                    parent,
                    n,
                    new_parent,
                    nn,
                    code
                );
                reply.error(code)
            }
        }
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let filename = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(libc::EINVAL);
                return;
            }
        };
        self.refresh_if_needed();
        match self.apply_lookup(parent, filename) {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(code) => reply.error(code),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        match self.apply_read(ino, offset as usize, size as usize) {
            Ok(data) => reply.data(&data),
            Err(code) => reply.error(code),
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.apply_readlink(ino) {
            Ok(target) => reply.data(&target),
            Err(code) => reply.error(code),
        }
    }

    fn symlink(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &std::path::Path,
        reply: ReplyEntry,
    ) {
        let (Some(name), Some(target_str)) = (link_name.to_str(), target.to_str()) else {
            reply.error(libc::EINVAL);
            return;
        };
        log::debug!(
            "SYMLINK parent={} name={:?} -> {:?}",
            parent,
            name,
            target_str
        );
        match self.apply_symlink(parent, name, target_str) {
            Ok((_, attr)) => reply.entry(&TTL, &attr, 0),
            Err(code) => reply.error(code),
        }
    }
}

fn file_attr(
    ino: u64,
    size: u64,
    perm: u16,
    uid: u32,
    gid: u32,
    time: SystemTime,
    kind: FileType,
) -> FileAttr {
    FileAttr {
        ino,
        size,
        // FUSE counts in 512-byte units, regardless of the reported blksize.
        blocks: size.div_ceil(512),
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind,
        perm,
        nlink: 1,
        uid,
        gid,
        rdev: 0,
        flags: 0,
        blksize: 4096,
    }
}

fn dir_attr(ino: u64, perm: u16, uid: u32, gid: u32, time: SystemTime) -> FileAttr {
    FileAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: time,
        mtime: time,
        ctime: time,
        crtime: time,
        kind: FileType::Directory,
        // POSIX convention: directory nlink = 2 (. and its name) + subdirs.
        // We simplify to 2 — some macOS apps check for > 1 and interpret 1 as
        // an "orphaned"/being-deleted directory and hide it from the UI.
        nlink: 2,
        perm,
        uid,
        gid,
        rdev: 0,
        flags: 0,
        blksize: 4096,
    }
}

/// Errno for "extended attribute does not exist". macOS uses `ENOATTR` (93);
/// Linux uses `ENODATA` (61). If we hardcode one of them, the other kernel
/// interprets it as a completely different error and may degrade FS behavior.
#[inline]
fn enoattr() -> i32 {
    #[cfg(target_os = "macos")]
    {
        libc::ENOATTR
    }
    #[cfg(not(target_os = "macos"))]
    {
        libc::ENODATA
    }
}

/// Fixed overhead of a sealed blob: magic (24) + nonce (24) + AEAD tag (16).
const SEALED_OVERHEAD: u64 = 64;

/// `decrypt_listing` with a disk->clear cache: paths already known reuse
/// their clear form; only new paths are decoded, reading just the sidecars
/// they need from the working tree. Returns the listing plus the cache for
/// exactly the current entries (stale paths drop out).
fn decrypt_listing_cached(
    crypto: Option<&crate::crypt::Crypto>,
    repo_path: &str,
    dirs: Vec<String>,
    files: Vec<git::ListedFile>,
    cache: &HashMap<String, String>,
) -> (Vec<String>, Vec<git::ListedFile>, HashMap<String, String>) {
    let Some(c) = crypto else {
        return (dirs, files, HashMap::new());
    };
    let mut new_cache: HashMap<String, String> =
        HashMap::with_capacity(dirs.len() + files.len());
    let root = std::path::Path::new(repo_path);
    let mut decode = |disk: &str| -> String {
        let clear = match cache.get(disk) {
            Some(clear) => clear.clone(),
            None => dec_path_reading_sidecars(c, root, disk),
        };
        new_cache.insert(disk.to_string(), clear.clone());
        clear
    };
    let dirs = dirs.into_iter().map(|d| decode(&d)).collect();
    let files = files
        .into_iter()
        .filter(|f| !crate::crypt::is_long_sidecar(leaf_name(&f.path)))
        .map(|mut f| {
            let clear = decode(&f.path);
            let decoded = clear != f.path;
            f.path = clear;
            if decoded && f.kind == git::FileKind::Regular {
                f.size = f.size.saturating_sub(SEALED_OVERHEAD);
            }
            f
        })
        .collect();
    (dirs, files, new_cache)
}

/// Decrypts an on-disk path, reading each hashed long-name component's
/// sidecar (exact `<leaf>.name`, or the conflict base `dslong_<h>.name`)
/// from the working tree on demand. Unresolvable components stay literal.
fn dec_path_reading_sidecars(
    c: &crate::crypt::Crypto,
    root: &std::path::Path,
    on_disk: &str,
) -> String {
    let mut clear: Vec<String> = Vec::new();
    let mut dir = std::path::PathBuf::new();
    for comp in on_disk.split('/') {
        if crate::crypt::is_long_name(comp) {
            let read = |name: &str| std::fs::read_to_string(root.join(&dir).join(name)).ok();
            let content = read(&crate::crypt::sidecar_name(comp)).or_else(|| {
                crate::crypt::long_name_base(comp)
                    .and_then(|base| read(&crate::crypt::sidecar_name(base)))
            });
            match content.and_then(|ct| c.dec_long(comp, ct.trim())) {
                Some(cl) => clear.push(cl),
                None => clear.push(comp.to_string()),
            }
        } else {
            clear.push(c.dec_path(comp));
        }
        dir.push(comp);
    }
    clear.join("/")
}

fn leaf_name(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}


#[cfg(test)]
mod tests {
    use super::BlobCache;

    #[test]
    fn blob_cache_counts_hits_and_misses() {
        let mut c = BlobCache::new(100);
        assert!(c.get(1).is_none());
        c.insert(1, b"abc".to_vec());
        assert_eq!(c.get(1).map(|d| d.as_slice()), Some(b"abc".as_slice()));
        assert_eq!(c.stats(), (1, 1));
    }

    #[test]
    fn decrypt_listing_resolves_long_name_and_hides_sidecar() {
        use super::{decrypt_listing_cached, git};
        use crate::crypt::Crypto;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap();
        let c = Crypto::from_master_key(&[3u8; 32]);

        let long = "q".repeat(200);
        let e = c.enc_component(&long);
        let disk_leaf = e.disk.clone(); // dslong_<hash>
        let sidecar_content = e.sidecar.clone().unwrap();
        // Materialize the sidecar as it exists in the worktree.
        std::fs::write(
            dir.path().join(format!("{}.name", disk_leaf)),
            &sidecar_content,
        )
        .unwrap();

        let files = vec![
            git::ListedFile {
                path: disk_leaf.clone(),
                size: 116,
                kind: git::FileKind::Regular,
            },
            git::ListedFile {
                path: format!("{}.name", disk_leaf),
                size: sidecar_content.len() as u64,
                kind: git::FileKind::Regular,
            },
        ];
        let (dirs, files, _) = decrypt_listing_cached(Some(&c), repo, vec![], files, &std::collections::HashMap::new());
        assert!(dirs.is_empty());
        // The sidecar is hidden and the long name resolves to its cleartext.
        assert_eq!(files.len(), 1, "sidecar must be hidden from the listing");
        assert_eq!(files[0].path, long);
    }

    #[test]
    fn blob_cache_evicts_lru_when_over_budget() {
        let mut c = BlobCache::new(10);
        c.insert(1, vec![0; 6]);
        c.insert(2, vec![0; 4]);
        // Access 1 so that 2 becomes the LRU
        assert!(c.get(1).is_some());
        c.insert(3, vec![0; 4]);
        assert!(c.contains(1));
        assert!(!c.contains(2), "LRU entry should have been evicted");
        assert!(c.contains(3));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn blob_cache_skips_blob_larger_than_budget() {
        let mut c = BlobCache::new(4);
        c.insert(1, vec![0; 5]);
        assert!(!c.contains(1));
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn blob_cache_clear_resets_entries() {
        let mut c = BlobCache::new(100);
        c.insert(1, vec![0; 10]);
        c.insert(2, vec![0; 10]);
        c.clear();
        assert_eq!(c.len(), 0);
        // Space freed: reinsert without eviction
        c.insert(3, vec![0; 90]);
        assert!(c.contains(3));
    }

    #[test]
    fn blob_cache_insert_same_inode_replaces_without_double_count() {
        let mut c = BlobCache::new(10);
        c.insert(1, vec![0; 6]);
        c.insert(1, vec![0; 8]);
        assert!(c.contains(1));
        assert_eq!(c.len(), 1);
        // If the total had counted 6+8, this insert would evict inode 1
        c.insert(2, vec![0; 2]);
        assert!(c.contains(1));
        assert!(c.contains(2));
    }
}
