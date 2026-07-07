# AGENTS.md

This file guides coding agents (and human contributors) working in this repository.

## Invariants (never violate)

- Drain correctness must NEVER depend on sync-queue coalescing — coalescing is only an economy on redundant content. The queue (`Vec<SyncOp>`) preserves FUSE kernel arrival order, which is the causal order; any "optimization" that reorders or drops ops reintroduces the shuffled chained-rename and write-before-rename bugs fixed in Phase 0 (data integrity).
- `checkout force` (and any merge/FF) may only run behind both guards: `repo_lock` acquired AND `is_working_tree_clean` true. A new checkout call site that skips the guards reintroduces silent loss of uncommitted local changes (incident: FF pull destroyed writes made in identity-less mode).
- An inode write buffer must NEVER start empty for an existing file — always a copy of the current content via `ensure_buffer` (POSIX semantics). Empty buffer + partial write zeroed the rest of the file on commit (append/truncate corruption, fixed in Phase 0).

## Known pitfalls

- `unwrap_or(expr)` ALWAYS evaluates `expr` (eager argument). With a side-effecting expression (e.g., `alloc_inode()`), the effect leaks even on the `Some` path — this was the inode leak in refresh. Use `unwrap_or_else`; the project's clippy does not catch this case by default.
- FUSE `flush` runs on EVERY `close(2)`, including fds opened read-only. Any flush logic that assumes "a write happened before" must check the dirty flag — without it, a simple `cat` re-enqueued stale content and reverted a remote merge.
- `git2::Remote::push` returns `Ok` at the transport level EVEN when the ref is rejected by the server (e.g., GitHub's GH001 for blobs > 100 MB) — the rejection only arrives through the `push_update_reference` callback. Incident symptom (2026-07-03): "push ok" logged with the remote stalled and reconcile never firing. A new push call site must collect callback rejections the way `push_to_remote` does (fix `fa43c2e`), otherwise the false positive returns.

## Verification commands

- Full suite: `cargo test`
- Lint (new warnings outside `vendor/` are a regression): `cargo clippy --all-targets`
- Phase 0 forbidden patterns: `grep -rn "op_priority" src/` and `grep -n "unwrap_or(&self.alloc_inode" src/fs.rs` must return nothing
- CI (`.github/workflows/ci.yml`) only MIRRORS these commands on push/PR. Decoupling clause
  (project decision, 2026-07-05): no code in `src/`/`tests/` may know about or depend on the CI
  (`grep -rnE 'GITHUB_' src/ tests/` must return nothing); the only automation beyond
  verification will be the release automation, in a future slice.

## Build & Run

```bash
cargo build                                      # compile
cargo test                                       # full suite (unit + integration)
cargo run -- -r <repo_path> -m <mount_path>      # mount the Git repository as FUSE
cargo run -- install -r <repo> -m <mount> [--with-tray]  # LaunchAgent (mount at login)
cargo run -- uninstall                           # stop the daemon and remove the agents
cargo run -- status                              # daemon state
cargo run -- tray                                # status icon in the menu bar
cargo run -- key generate [--out <path>] [--force]  # at-rest encryption key
```

Mount-mode flags: `--config`, `--log-dir`, `--status-file`. Config resolution without the flag:
`./config.json` (dev) > `~/.config/drive-sync/config.json` (canonical, with the encryption key
next to it) > legacy location in `~/Library/Application Support/drive-sync/` (`install` migrates).
Status stays in `~/Library/Application Support/drive-sync/`, logs in `~/Library/Logs/drive-sync/`.

Requires macFUSE on macOS (`brew install macfuse`). The mount directory must exist before the command.

## Overview

drive-sync is a FUSE filesystem in Rust that exposes files of a Git repository as a mountable
filesystem, with bidirectional sync via Git: local writes become automatic commits + pushes and
remote changes arrive via periodic pull. It behaves like a "Dropbox-style" drive where the remote
is a Git repository (including GitHub).

**Main flow:** CLI -> `Config::load_from` -> `SyncManager::new_full_with_pull` (write watcher + pull watcher) -> `GitDriveFS::new` -> `fuser::mount2`.

## Modules

| File | Role |
| --- | --- |
| `src/main.rs` | Synchronous entry point with subcommand dispatch (`status`/`install`/`uninstall`/`tray`). Mount mode: explicit tokio runtime, `prepare_mountpoint` (recovers a zombie mount after a crash), `fuser::spawn_mount2`, SIGTERM/SIGINT/eject wait and ordered shutdown (unmount -> `drain_now` -> status/stop notification). |
| `src/status.rs` | Observable daemon state: serializable `SyncStatus` (unix timestamps, mount, log_file, stopped_at), `StatusState` with boot/drain/pull/stop events, atomic write and the `status` subcommand's `format_status`. |
| `src/paths.rs` | Path resolution: CWD-relative defaults (dev) and macOS defaults (launchd); status discovery order for reads. |
| `src/launchd.rs` | LaunchAgents: pure `render_plist`/`render_tray_plist` (template + XML escaping), install/uninstall via `launchctl bootstrap/bootout`, KeepAlive `{SuccessfulExit=false}`. |
| `src/notify.rs` | macOS notifications via `osascript`: pure formatting, `Notifier` off by default, `RateLimiter` (first + 1/h) for persistent events. |
| `src/tray.rs` | Menu bar icon (`tray-icon` + `tao`): status.json polling, 4 visual states mapped by `evaluate` (pure), menu with Finder/log tail/status/quit. |
| `src/lib.rs` | Re-exports modules to enable integration tests (`tests/*.rs`). |
| `src/fs.rs` | `GitDriveFS` implements `fuser::Filesystem`. Hierarchical model with `FileEntry`/`DirEntry` (kind Regular/Symlink and local-only flag), atomic inode allocator, `apply_*` functions for pure testable logic (includes `apply_write`/`apply_truncate`/`apply_flush`/`apply_readlink`/`apply_symlink`). Write buffers initialized from HEAD with a per-inode dirty flag — copy-on-write RAM (`Arc` shared with the queue) below `large_file_threshold_mb`, spool file in `.git/drive-sync-spool/` above; gc of clean buffers when `commit_generation` advances or on post-pull refresh. Ignored working-tree files appear as visible local-only entries (boot/refresh/gc). mtime derived from the last commit per path (`derived_times`); reads on cache miss use a persistent repo handle. `setattr` + in-memory xattrs (`mode_overrides`, `times`, `xattrs`). |
| `src/git.rs` | Wrapper over `git2`. Besides reads (`list_files_at_commit` returns `ListedFile` with `FileKind` Regular/Symlink via filemode 120000), exposes `current_branch_name`, `list_dirs_at_commit`, `list_ignored_files` (local-only candidates), `commit_times_by_path` (single mtime revwalk with a ceiling), `commit_working_tree` (the `add_all` with `DEFAULT` honors the mounted repo's `.gitignore` — specified and tested behavior), `push_to_remote`, `fetch_remote`, `merge_ff_only`, `pull_and_merge` (3-way + conflicted copy), `is_working_tree_clean` (pull guard; ignored files do not block) and the `MergeOutcome` enum (`NoOp`/`FastForwarded`/`Merged`/`Conflicted`). |
| `src/sync.rs` | `SyncManager` with the `SyncOp` enum (Write/Delete/Mkdir/Rmdir/Rename/Symlink) and a causal queue in arrival order (`push_coalescing`: Write coalesces in-place; rename/delete/mkdir/rmdir/symlink are barriers). `SyncOp::Write` carries `WriteContent` — `Mem(Arc)` shared with the write buffer or `Spooled(PathBuf)` applied by rename at drain. Debounce watcher applies operations + commit + push; the periodic pull watcher signals refresh via `AtomicBool`. The two cycles are serialized by `repo_lock`; pull requires a clean working tree (`is_working_tree_clean`). Filters ephemeral macOS paths (`._*`, `.DS_Store`, `.Spotlight-V100`, `.Trashes`, etc.) in `enqueue`. |
| `src/config.rs` | `Config::load_from` loads `config.json` with silent fallback to defaults. Fields: `write_debounce_secs`, `pull_interval_secs`, `read_cache_mb`, `large_file_threshold_mb`, `chunk_threshold_mb`, `chunk_size_mb`, `user_name`, `user_email`, `remote_token`. |
| `src/chunk.rs` | Pure chunking core (Phase 2.6): versioned text manifest (`drive-sync-chunked v1`), `.chunks/<2 hex>/<sha256>` sharding, streaming slicing with dedup, offset->chunks mapping. |
| `src/crypt.rs` | At-rest encryption (Phase 2.7): convergent XChaCha20-Poly1305 (nonce derived from the plaintext — dedup and stable Git blob), HKDF subkeys, per-component names in lowercase base32 with literal fallback, 0600 key file. The repo stores everything encrypted; FUSE presents plaintext. |

## Main structures

- `FileEntry` (`src/fs.rs`) — `inode`, `path` (full), `parent`, `size`, `kind` (Regular/Symlink), `local_only`.
- `DirEntry` (`src/fs.rs`) — `inode`, `path`, `parent`, `children: HashSet<u64>`.
- `GitDriveFS` (`src/fs.rs`) — bidirectional indices (`files_by_inode`, `dirs_by_inode`, `inodes_by_path`), `next_inode: Arc<AtomicU64>`, per-inode write buffers (`WriteBuffer` RAM/spool), persistent read repo handle, `derived_times`, `SyncManager`.
- `SyncOp` (`src/sync.rs`) — enum with six variants covering all mutations; `Vec<SyncOp>` queue in arrival order.
- `SyncManager` (`src/sync.rs`) — two async watchers (debounce-commit-push and pull) serialized by `repo_lock: Arc<Mutex<()>>`, optional identity and credentials, `refresh_flag: Arc<AtomicBool>`, `commit_generation: Arc<AtomicU64>`.
- `Config` (`src/config.rs`) — 7 fields with `#[serde(default)]`.

## Configuration

`config.json` at the project root. If absent or invalid, defaults are applied silently (with a warning log).

| Field | Type | Default | Description |
| --- | --- | --- | --- |
| `write_debounce_secs` | `u64` | `5` | Seconds of idleness before draining the buffer, applying ops, committing and pushing. |
| `pull_interval_secs` | `u64` | `60` | Interval between `git fetch` + merge attempts against the `origin` remote. `0` disables the pull watcher. |
| `read_cache_mb` | `u64` | `64` | LRU read blob cache budget, in MiB. `0` disables it (every read goes to the repo, via the persistent handle). |
| `chunk_threshold_mb` | `u64` | `95` | Files above this are committed as manifest + chunks in `.chunks/` (no blob above the threshold is staged — GitHub's 100 MB/file limit). `0` disables. |
| `chunk_size_mb` | `u64` | `80` | Maximum size of each chunk. |
| `encryption_enabled` | `bool` | `false` | At-rest encryption: content AND names encrypted in the repo (FUSE presents plaintext). Changing the value migrates the worktree on the next drain — prior history stays as it was. Enabled without a readable key ABORTS boot. |
| `encryption_key_file` | `Option<String>` | `None` | Master key file (64 hex, 0600). Absent = `~/.config/drive-sync/drive.key` (read fallback in the legacy location). Generate with `drive-sync key generate`. LOSING THE KEY = LOSING THE DATA. |
| `large_file_threshold_mb` | `u64` | `64` | Threshold above which write buffers go to a disk spool file (`.git/drive-sync-spool/`) instead of RAM. `0` disables the spool. |
| `user_name` | `Option<String>` | `None` | Identity used in commits. If absent, commits are skipped (the working tree keeps updating). |
| `user_email` | `Option<String>` | `None` | Same treatment as `user_name`. |
| `remote_token` | `Option<String>` | `None` | HTTPS token (e.g., GitHub PAT). If absent, uses the system ssh-agent/credential helper. **Plain text — keep `config.json` in `.gitignore`.** |

## End-to-end flow

1. FUSE write (`write`) -> `apply_write`: the inode buffer starts as a copy of the HEAD blob (POSIX semantics for partial writes/appends) and the inode is marked dirty.
2. `flush` -> `apply_flush`: enqueues `SyncOp::Write` in the `SyncManager` **only if dirty** (closing a read-only fd is a no-op) and clears the mark. The content is a shared snapshot (`Arc`) or a disk-to-disk copy of the spool — never a full in-RAM copy.
3. The debounce watcher drains the queue after `write_debounce_secs` of idleness and holds `repo_lock` for the whole cycle.
4. Each `SyncOp` is applied to the working tree **in arrival order** (`fs::write` or spool rename, `fs::rename`, `mkdir -p + .gitkeep`, real symlink, etc.). After application, the chunking sweep converts any raw file above `chunk_threshold_mb` into manifest + `.chunks/` and the GC removes chunks no manifest references.
5. If an identity is present, `git::commit_working_tree` creates **1 commit** with message `drive-sync: N changes (p1, p2, p3[, +M more])` and increments `commit_generation`. Paths matching the mounted repo's `.gitignore` stay out of the commit — in the mount they remain visible as local-only.
6. If `remote_name` is present, `git::push_to_remote` publishes to the branch. Failure triggers `reconcile_and_retry_push` (fetch + `pull_and_merge` + retry). The local commit is never reverted.
7. In parallel, the pull watcher every `pull_interval_secs` — under `repo_lock` and **only with a clean working tree** — runs `git::fetch_remote` + `git::pull_and_merge` (or `merge_ff_only` when identity-less). On any outcome that touches the tree (FF/Merged/Conflicted), it sets `refresh_flag`.
8. `GitDriveFS::refresh_if_needed` (called in `lookup`/`getattr`/`readdir`) discards non-dirty buffers when `commit_generation` advanced or on post-pull refresh, and rebuilds indices preserving inodes of still-existing paths.
9. Shutdown (SIGTERM/SIGINT/eject): unmount BEFORE `drain_now` — the kernel delivers flushes of open fds, the final drain commits/pushes everything, the status records the stop and the daemon exits 0 (launchd KeepAlive does not resurrect a clean stop; a crash relaunches and boot recovers the zombie mount).

## Tests

```bash
cargo test           # all tests (unit + integration)
cargo test --lib     # unit tests only
cargo test --test fs_hierarchy   # hierarchical model and apply_* tests
cargo test --test sync_flow      # watcher -> disk -> commit -> push -> pull flow
cargo test --test e2e_integration # end-to-end scenarios
```

The suite covers ~253 tests: 110 unit (`config`, `git` — including symlink/ignore/mtime —, `sync`, `BlobCache`, `status`, `paths`, `launchd`, `notify`, `tray::evaluate`), 98 for hierarchy/apply_\*, buffers (RAM and spool), cache, symlinks, local-only, derived mtime, chunked and encrypted reads, ephemeral renames, `.gitkeep` and `.Trashes` (`fs`), 39 end-to-end flow (drain, causal batches, pull watcher, dirty-tree guard, `drain_now`, status.json, symlink/ignored/spool through commit), 4 E2E, 1 smoke. **End-to-end tests through the FUSE kernel are manual** — the `apply_*` methods allow testing all the logic without mounting FUSE.

## Performance notes

- `apply_read` (`src/fs.rs`) consults the buffer (RAM or spool), then the LRU blob cache (`read_cache_mb`) and only then the repo — through a persistent handle opened once (invalidated on error). Files larger than the budget do not enter the cache, but do not pay one repo open per syscall.
- `refresh_if_needed` is called on every `lookup`/`getattr`/`readdir`. Cheap when the flag is `false` (atomic read); expensive when `true` (rebuilds entire indices).
- The pull watcher skips an iteration if a flush batch is pending — avoids competing with local commits.

## Known limitations

1. **Automatic `.gitkeep` on `mkdir`.** Git does not track empty directories; drive-sync creates/removes `.gitkeep` to keep the illusion.
2. **Plain-text credentials** in `config.json`. `.gitignore` covers the file by default. A keychain mechanism is future work.
3. **3-way merge with conflicted copy.** Pull uses `pull_and_merge` when an identity is configured: FF if possible, otherwise 3-way merge; on conflict, the local version goes to `<path>.conflict-<host>-<ts>` and the remote one becomes canonical (merge commit with 2 parents). Identity-less, it degrades to `merge_ff_only` (stalls on divergence).
    **Pull requires a clean working tree.** With uncommitted changes, the iteration is skipped — in identity-less mode (commits skipped), the first local write blocks pull until a manual commit or identity configuration. Sync becomes effectively unidirectional in that mode.
4. **Push failed -> automatic retry.** If a push is rejected (typically non-FF), `reconcile_and_retry_push` does fetch + `pull_and_merge` + push again. Persistent failure only logs; the local commit is never reverted.
5. **Rename detection depends on Git's heuristic.** We do not use `git mv`; the working tree sees delete + create.
6. **Symlinks supported; no FIFOs, sockets or real hardlinks** (`link` is a copy). Symlink targets are opaque (no-follow, never validated): an absolute target created on one machine is broken on another, as in plain Git. Repos with LFS show the text pointer (no smudge/clean filters).
6b. **Files above `chunk_threshold_mb` become manifest + `.chunks/`** (Phase 2.6): transparent in the mount (real size, reads reassembled with sha256 verification); a plain Git clone sees the text manifest and the chunks (manual recovery: concatenate the chunks in manifest order). `.chunks` at the root is reserved. No incremental re-chunking (CDC): editing a large file rewrites the chunks touched by position.
7. **Permission metadata in memory.** Exposed `uid`/`gid` are those of the process that mounted FUSE (via `libc::getuid()`/`getgid()`). `chmod` is accepted and stored in `mode_overrides` (per-inode) but **does not persist** across mounts — Git only tracks `0o644`/`0o755`. `chown` is silently accepted and ignored.
8. **mtime derived from history; session touches in memory.** An untouched file exposes the timestamp of the last commit that touched the path (single revwalk at boot/refresh, 4096-commit ceiling, the remainder falls back to HEAD's timestamp); `touch` in `flush`/`setattr` takes precedence and does not persist across mounts. `atime`/`ctime`/`crtime` mirror the available mtime. Local-only entries and directories use `boot_time`. Stable mtime across `getattr`s is what unblocks saving in macOS apps that detect "external modification" via stat.
9. **xattrs in memory.** `setxattr`/`getxattr`/`listxattr`/`removexattr` store in `HashMap<u64, HashMap<OsString, Vec<u8>>>`. Lost on remount — macOS Versions only works while the mount is alive. Git does not store arbitrary xattrs.
10. **Detached HEAD aborts boot.** drive-sync requires a named branch.
11. **The blob cache does not cover files larger than the budget** (`read_cache_mb`) — for them every read goes to the repo, but via the persistent handle (one open per mount). Write buffers above `large_file_threshold_mb` live in an on-disk spool.
12. **Refresh only triggers on `lookup`/`getattr`/`readdir`.** If the kernel caches `getattr` (1s TTL), remote changes can take up to that TTL to appear.
13. **Ambiguous implicit swap** (more than one file at the origin) returns `EXDEV` and the app degrades to a copy.
14. **The Finder Trash lives in memory.** Items moved to the trash disappear from the repo and are recoverable only while the mount lives — a remount "empties" the Trash. `.gitkeep` is invisible in the mount (an internal artifact of the empty-dir illusion).
15. **Notifications attributed to "Script Editor"** (osascript, no app bundle). Migration to UserNotifications when there is a bundle (roadmap).
16. **Optional at-rest encryption (Phase 2.7).** With `encryption_enabled`, content and names live encrypted in the repo (working tree included) and the mount presents everything in plaintext. Convergent by design (dedup and stable tree; equality oracle accepted). What it does NOT protect: pre-migration history, approximate sizes, commit count/cadence, the local machine. Ignore/local-only are disabled in encrypted mode; plaintext component names limited to ~119 bytes; lost key = unrecoverable data (no backdoor). Multi-machine = copy the key file.
17. **Ignored files are visible local-only entries.** Paths matching the mounted repo's `.gitignore` appear in the drive and persist in the working tree across remounts, but never enter a commit/push — they do not sync between machines. The exclusion mechanism is `.gitignore` itself (or `.git/info/exclude` for local policy); there is no allowlist. An ignore rule removed remotely makes the file get committed on the next drain (auto-resolve).

## Language

All user-facing strings (CLI help, logs, notifications) and all code comments are in English. Portuguese documentation lives in `docs/pt-br/`.

## Detailed documentation

- [docs/usage.md](./docs/usage.md) — step-by-step usage guide (prerequisites, credentials, `config.json`, mount, day-to-day, troubleshooting)
- [docs/architecture.md](./docs/architecture.md) — overview, layers and data structures
- [docs/sync-flow.md](./docs/sync-flow.md) — debounce and pull watchers, commit, push, refresh
- [docs/fuse-filesystem.md](./docs/fuse-filesystem.md) — hierarchical inode mapping and FUSE operations
- Portuguese versions: [docs/pt-br/](./docs/pt-br/)
