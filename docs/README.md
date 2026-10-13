# Technical documentation — git-drive

Architectural documentation for the project. For build/run and a high-level overview, see the
root [README](../README.md); for agent/contributor guidance, [AGENTS.md](../AGENTS.md).

## Index

- [usage.md](./usage.md) — Step-by-step guide: prerequisites, credentials, `config.json`, mounting, day-to-day, symlinks, ignored files, large files (chunking), encryption at rest and troubleshooting
- [architecture.md](./architecture.md) — Project overview, layers (including chunking and encryption), data structures and known limitations
- [sync-flow.md](./sync-flow.md) — `SyncManager`, causal queue, drain (chunking, encryption and migrations), pull watcher and push reconcile
- [fuse-filesystem.md](./fuse-filesystem.md) — Inode-path mapping, implemented FUSE operations, buffers (RAM/spool) and read/write flows
- [large-file-reads.md](./large-file-reads.md) — Range reads of large files: once-per-session verification, range decryption, measured performance on macOS and Linux, how to extend and re-measure

Portuguese versions of all documents live in [pt-br/](./pt-br/).
