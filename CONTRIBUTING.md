# Contributing to git-drive

Thanks for your interest in contributing. git-drive is open source under the
[MIT license](./LICENSE); contributions of any size are welcome — bug reports,
docs, tests, features.

## Ways to contribute

- **Bug reports.** Open an issue with: what you did, what you expected, what
  happened, the output of `git-drive status`, the relevant excerpt from
  `~/Library/Logs/git-drive/`, and your macOS + macFUSE versions. If the bug
  involves sync, say whether the remote is GitHub, and whether encryption or
  chunking were enabled.
- **Documentation.** English docs in `docs/` are canonical; the Portuguese
  mirror in `docs/pt-br/` often lags behind — syncing it is a great
  contribution. So are new troubleshooting entries in `docs/usage.md` for
  problems you actually hit.
- **Tests.** The suite runs without a FUSE mount, so improving coverage of the
  pure `apply_*` logic is low-friction and highly valued.
- **Features.** For anything non-trivial, open an issue describing the
  approach before writing code — priorities move, and an early conversation
  avoids wasted work.

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

git-drive is experimental and sits between the kernel and your data: when
testing a mount manually, always use a **disposable repository**, never the
only copy of anything you care about.

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

## Opening the PR

- Fork, create a branch off `main`, and keep the PR focused on one change —
  small PRs get reviewed fast, mixed ones stall.
- Use an imperative title in the commit style below (`fix: ...`, `feat: ...`).
- In the description, state the behavior change and point at the tests that
  cover it. Link the issue if one exists.
- CI must be green; a PR that touches an invariant documented in
  [AGENTS.md](./AGENTS.md) should say so explicitly and justify it.

## Good first contributions

- Sync a `docs/pt-br/` page that fell behind its English counterpart.
- Add a troubleshooting entry to `docs/usage.md` for a failure mode you hit.
- Extend test coverage of edge cases in the pure logic (chunking boundaries,
  path resolution, merge conflict naming).
- The Linux runtime port is the standing big feature (the test suite already
  passes on Linux) — open an issue first to coordinate.

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

Do not open a public issue for an exploitable vulnerability. Use GitHub's
[private vulnerability reporting](https://github.com/giacomeli/git-drive/security/advisories/new)
(Security tab > Report a vulnerability), or reach out to the maintainer
privately (see the commit history for contact). See [SECURITY.md](./SECURITY.md).
