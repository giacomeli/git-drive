# QWEN.md

Instructional context for Qwen Code when working in this repository.
[AGENTS.md](./AGENTS.md) is the canonical deep guide (full module table, config reference,
known limitations) — this file summarizes what matters most for making changes safely.

## Project Overview

**git-drive** is a FUSE filesystem in Rust that mounts a Git repository as a Dropbox-style
drive: local writes become automatic commits + pushes, remote changes arrive via periodic
pull. Optional at-rest encryption (XChaCha20-Poly1305, convergent) lets even a public repo
act as a private drive. Runs on macOS (macFUSE) and Linux (fuse3); `git-drive install`
creates the login service on both (LaunchAgent / systemd user units).

- Crate: `git-drive` v0.2.0, Rust edition 2021, MSRV **1.85** (checked in CI)
- Key deps: `fuser` 0.14 (locally patched, see `vendor/`), `git2`, `tokio`, `clap`,
  `chacha20poly1305`/`hkdf`/`sha2` (encryption + chunking), `tray-icon`/`tao` (menu bar),
  `eframe`/egui (viewer window)
- Main flow: CLI → `Config::try_load_from` (invalid config aborts boot) →
  `SyncManager::new_full_with_pull` → journal replay + `recover_at_boot` → `GitDriveFS::new`
  → `fuser::spawn_mount2`

## Build, Run & Verification

```bash
cargo build                                       # compile
cargo test                                        # full suite (~315 tests, unit + integration)
cargo clippy --all-targets                        # warnings outside vendor/ = regression
cargo run -- -r <repo_path> -m <mount_path>       # mount a Git repo as FUSE
cargo run -- status                               # daemon state
cargo run -- tray                                 # menu bar status icon
cargo run -- viewer                               # egui window: status + live log tail
cargo run -- install -r <repo> -m <mount> [--with-tray]  # LaunchAgent (macOS) / systemd user units (Linux)
cargo run -- uninstall                            # stop daemon, remove the service
cargo run -- key generate [--out <path>] [--force]       # encryption key
```

Phase 0 forbidden patterns (must return nothing — CI enforces):

```bash
grep -rn "op_priority" src/
grep -n "unwrap_or(&self.alloc_inode" src/fs.rs
grep -rnE 'GITHUB_' src/ tests/    # decoupling clause: no code may know about CI
```

Focused test runs: `cargo test --lib` (unit), `cargo test --test fs_hierarchy`,
`cargo test --test sync_flow`, `cargo test --test e2e_integration`. End-to-end tests through
the FUSE kernel are **manual** — the pure `apply_*` methods in `src/fs.rs` allow testing all
logic without mounting.

## Invariants (NEVER violate)

These encode real data-loss incidents:

1. **Drain correctness never depends on sync-queue coalescing.** The queue (`Vec<SyncOp>`)
   preserves FUSE kernel arrival order (the causal order). Coalescing is only an economy on
   redundant content; any reordering/dropping reintroduces shuffled-rename and
   write-before-rename corruption bugs.
2. **`checkout force` (and any merge/FF) only behind both guards**: `repo_lock` acquired AND
   `is_working_tree_clean` true. Skipping the guards silently destroys uncommitted local
   changes.
3. **An inode write buffer must NEVER start empty for an existing file** — always a copy of
   current content via `ensure_buffer` (POSIX partial-write semantics). Empty buffer + partial
   write zeroed the rest of the file on commit.
4. **An op is journaled before it enters the queue, and its journal record is deleted only
   after the drain that took it has committed (under `repo_lock`).** Acknowledging a write
   that exists only in RAM lost files saved seconds before a crash.

## Known Pitfalls

- `unwrap_or(expr)` evaluates `expr` eagerly — side effects (e.g. `alloc_inode()`) leak even
  on the `Some` path. Use `unwrap_or_else`. Clippy does not catch this here.
- FUSE `flush` runs on EVERY `close(2)`, including read-only fds. Flush logic must check the
  dirty flag, or a plain `cat` re-enqueues stale content and reverts a remote merge.
- libgit2 calls the credential callback AGAIN after every failed attempt; a callback that
  always returns the agent loops until the server drops the connection (weeks of failed
  fetches). Track attempts — or use the default `git` CLI transport.
- `git2::Remote::push` returns `Ok` even when the server rejects the ref (e.g. GH001 for
  blobs > 100 MB) — rejection only arrives via the `push_update_reference` callback. New push
  call sites must collect callback rejections the way `push_to_remote` does.

## Architecture

| Module | Role |
| --- | --- |
| `src/main.rs` | CLI dispatch (`status`/`install`/`uninstall`/`tray`/`viewer`); mount mode resolves the git transport, enables the journal and runs boot recovery BEFORE mounting, `prepare_mountpoint` (zombie recovery), ordered shutdown: unmount → `drain_now` → status/notify |
| `src/fs.rs` | `GitDriveFS` implements `fuser::Filesystem`: inode model (`FileEntry`/`DirEntry`), pure `apply_*` functions, copy-on-write write buffers (RAM `Arc` or spool file), LRU read cache, local-only (ignored) entries, derived mtimes, in-memory xattrs/mode overrides |
| `src/sync.rs` | `SyncManager`: `SyncOp` enum (Write/Delete/Mkdir/Rmdir/Rename/Symlink), causal queue with `push_coalescing` (Write coalesces in place; rename/delete/mkdir/rmdir/symlink are barriers), debounce commit+push watcher and periodic pull watcher serialized by `repo_lock`; the pull watcher also publishes local commits the remote lacks; reconcile retries up to 3 rounds on concurrent pushes |
| `src/journal.rs` | Write-ahead journal of the queue (`.git/git-drive-journal/`): ops persisted before entering the queue, truncated after their drain commits, replayed at boot (crash durability) |
| `src/git.rs` | `git2` wrapper: listing at commit (sizes from object headers), `commit_working_tree`, network transport (`git` CLI by default via `run_git_net`; libgit2 fallback with a stateful credential callback), push with rejection detection, fetch/merge (`MergeOutcome`; modify/delete keeps the edit; long-name sidecars restored), `is_working_tree_clean` |
| `src/chunk.rs` | Pure chunking core: files above `chunk_threshold_mb` become a versioned text manifest + `.chunks/<2hex>/<sha256>` shards (dedup, GitHub 100 MB limit) |
| `src/crypt.rs` | At-rest encryption: convergent XChaCha20-Poly1305 (nonce from plaintext → stable blobs + dedup), HKDF subkeys, encrypted component names |
| `src/config.rs` | `Config::try_load_from` (mount mode: missing file = defaults, invalid file = error) and the lenient `Config::load_from` |
| `src/status.rs` | Serializable `SyncStatus` written atomically to status.json; `status` subcommand formatting |
| `src/service.rs` | Platform-neutral install steps: binary copied to `~/.local/bin/git-drive`, legacy config migration, config seeding; dispatches to launchd/systemd |
| `src/launchd.rs` | macOS LaunchAgents: pure `render_plist`, install/uninstall via `launchctl`, KeepAlive on crash |
| `src/systemd.rs` | Linux systemd user units (`git-drive.service`, `git-drive-tray.service`): pure renderers, install/uninstall via `systemctl --user` |
| `src/notify.rs` | Notifications (`osascript` on macOS, `notify-send` on Linux), rate-limited; `reap_in_background` for spawned helpers |
| `src/tray.rs` | Menu bar icon (`tray-icon` + `tao`): polls status.json, 4 visual states, menu actions |
| `src/viewer.rs` | egui/eframe window (`git-drive viewer`): status + bounded live log tail; separate process spawned by the tray (avoids clashing with tao's event loop) |
| `src/lib.rs` | Re-exports modules for integration tests (`tests/*.rs`) |
| `build.rs` | Embeds `GIT_DRIVE_BUILD` identifier (env → `git rev-parse` → `unknown`) |
| `vendor/fuser` | Local patch on fuser 0.14.0 fixing a SETATTR crtime panic (`Instant + Duration` overflow) that killed the daemon on Finder paste; wired via `[patch.crates-io]` — keep the patch |

### End-to-end flow (abbreviated)

FUSE `write` → `apply_write` (buffer = copy of HEAD blob, inode marked dirty) → `flush`
enqueues `SyncOp::Write` **only if dirty** → debounce watcher drains the queue **in arrival
order** under `repo_lock`, applies ops to the worktree, runs the chunking sweep, commits (1
commit per size-bounded batch), pushes (with reconcile-and-retry on rejection). Pull watcher
fetches + merges on the interval, only with a clean tree, and sets `refresh_flag`;
`refresh_if_needed` (on lookup/getattr/readdir) rebuilds indices and discards non-dirty
buffers.

## Conventions

- **Language:** all code, comments, CLI help, logs and notifications in English. Portuguese
  docs live in `docs/pt-br/` (commit messages in this repo's history are often pt-BR).
- **Testing:** pure functions (`apply_*`, `render_plist`, `evaluate`, chunk/crypt cores) are
  unit-tested without FUSE or network; every behavioral change ships with tests that fail
  without it. `tempfile` for repos, `serial_test` where global state requires it.
- **CI decoupling:** `.github/workflows/ci.yml` only mirrors the local verification commands;
  no code in `src/`/`tests/` may reference CI environment variables.
- **Clippy bar:** new warnings outside `vendor/` are a regression (CI fails on them).
- Don't edit `vendor/` except deliberately maintaining the fuser patch (marked
  "PATCH local" in `vendor/fuser/src/ll/request.rs`).
- `gitmount/` and `specs/` are currently empty; `promo-assets/` holds README images;
  `docs/` holds usage/architecture/sync-flow/fuse guides (keep them in sync with behavior
  changes).

## Configuration Reference (quick)

`config.json`: `./config.json` (dev) > `~/.config/git-drive/config.json` (canonical) > legacy
`~/Library/Application Support/git-drive/`. Fields (defaults): `write_debounce_secs` (5),
`pull_interval_secs` (60, 0 disables), `read_cache_mb` (64), `large_file_threshold_mb` (64,
spool above), `chunk_threshold_mb` (95), `chunk_size_mb` (80), `commit_batch_mb` (256),
`encryption_enabled` (false — toggling migrates the worktree on next drain),
`encryption_key_file` (null → `~/.config/git-drive/drive.key`), `user_name`/`user_email`
(null → commits skipped, identity-less mode), `remote_token` (null; plain text — keep
`config.json` gitignored), `git_transport` ("auto": system `git` CLI, else libgit2),
`log_max_mb` (50), `log_keep_files` (10), `log_level` ("info",
`RUST_LOG` overrides). See AGENTS.md for full semantics.
