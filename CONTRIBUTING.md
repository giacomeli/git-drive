# Contributing to drive-sync

Thanks for your interest in contributing. drive-sync is open source under the
[MIT license](./LICENSE); contributions of any size are welcome — bug reports,
docs, tests, features.

## Getting started

```bash
git clone https://github.com/giacomeli/git-drive.git
cd git-drive
cargo build
cargo test          # full suite (unit + integration), no FUSE mount required
```

macFUSE (`brew install macfuse`) is only needed to mount a drive for manual
end-to-end testing — the suite exercises all the logic through pure `apply_*`
methods without touching the kernel.

## Before opening a PR

Read [AGENTS.md](./AGENTS.md) first. It documents the project invariants and
known pitfalls — every one of them encodes a real incident (data corruption,
silent data loss, false-positive pushes). A PR that violates an invariant will
not be merged, no matter how clean the code is.

Then make sure the local verification passes:

```bash
cargo test                       # all tests green
cargo clippy --all-targets       # no new warnings outside vendor/
grep -rn "op_priority" src/                      # must be empty
grep -n "unwrap_or(&self.alloc_inode" src/fs.rs  # must be empty
grep -rnE 'GITHUB_' src/ tests/                  # must be empty
```

The CI mirrors exactly these commands on macOS and Linux.

## Use of AI

AI-assisted contributions are accepted. Two conditions apply, and they are
enforced in review:

1. **Concise commits.** Write short, imperative commit messages that describe
   the change (`fix: ...`, `feat: ...`, `docs: ...`, `chore: ...`). No
   AI attribution, no `Co-Authored-By` trailers, no generated walls of text.
2. **Tests.** Every behavioral change must come with tests that fail without
   it. If an AI wrote the code, you are still the author: understand it, run
   it, and be able to defend it in review.

## Conventions

- Code identifiers, comments and user-facing strings are in English.
- Documentation is English-first (`docs/`); a Portuguese mirror lives in
  `docs/pt-br/`. Updating the English doc is required, mirroring to pt-BR is
  appreciated.
- Diagrams are Mermaid — no ASCII-art boxes.
- No emojis anywhere: code, docs, commits, PRs.
- New behavior belongs with its module: pure logic in `apply_*`-style
  functions (testable without FUSE), side effects at the edges.

## Reporting security issues

Do not open a public issue for an exploitable vulnerability. Reach out to the
maintainer privately first (see the commit history for contact).
