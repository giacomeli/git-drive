# Security Policy

## Supported versions

git-drive is experimental, pre-1.0 software. Only the latest commit on `main`
receives fixes; there are no maintained release branches.

## Reporting a vulnerability

Do not open a public issue for an exploitable vulnerability. Instead:

- Use GitHub's [private vulnerability reporting](https://github.com/giacomeli/git-drive/security/advisories/new)
  (Security tab > Report a vulnerability), or
- Reach out to the maintainer privately (see the commit history for contact).

Please include reproduction steps and the impact you believe the issue has
(data exposure, data loss, privilege escalation). You will get an initial
response as soon as possible; this is a spare-time project, so allow a few
days.

## Scope notes

- `config.json` may hold a plain-text `remote_token`, and the encryption key
  lives next to it in `~/.config/git-drive/` — protection of that directory is
  the user's responsibility (documented in the README).
- The threat model for encryption at rest: it protects repo content and file
  names from whoever can read the remote, not from an attacker with access to
  the machine where the drive is mounted (there, everything is plaintext).
