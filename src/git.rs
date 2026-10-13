use git2::{Repository, ObjectType, Error};
use std::path::Path;


pub fn open_repo(path: &str) -> Result<Repository, Error> {
    Repository::open(path)
}

pub fn get_commit<'repo>(repo: &'repo Repository, revision: &str) -> Result<git2::Commit<'repo>, Error> {
    let obj = repo.revparse_single(revision)?;
    obj.peel_to_commit()
}

pub fn read_file_at_commit(repo: &Repository, commit: &git2::Commit, file_path: &str) -> Result<Vec<u8>, Error> {
    let tree = commit.tree()?;
    let entry = tree.get_path(Path::new(file_path))?;
    let object = entry.to_object(repo)?;
    let blob = object.as_blob().ok_or_else(|| Error::from_str("Not a blob"))?;
    Ok(blob.content().to_vec())
}

/// Kind of a file entry in a Git tree. A symlink is a blob with filemode
/// 120000 whose content is the target text — without distinguishing the
/// filemode, a symlink would be flattened into a regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Regular,
    Symlink,
}

/// File entry listed from a tree (blob), with its full path.
#[derive(Debug, Clone)]
pub struct ListedFile {
    pub path: String,
    pub size: u64,
    pub kind: FileKind,
}

/// Symlink filemode in Git trees (S_IFLNK).
const FILEMODE_SYMLINK: i32 = 0o120000;

pub fn list_files_at_commit(
    repo: &Repository,
    commit: &git2::Commit,
) -> Result<Vec<ListedFile>, Error> {
    let tree = commit.tree()?;
    let odb = repo.odb()?;
    let mut files = Vec::new();

    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Some(name) = entry.name() {
            if let Some(ObjectType::Blob) = entry.kind() {
                // Size from the object HEADER: loading every blob just to
                // read its length decompressed the whole repository on each
                // boot and on every post-pull refresh (~2 GB on the real
                // drive — tens of seconds with the FUSE thread blocked).
                if let Ok((size, ObjectType::Blob)) = odb.read_header(entry.id()) {
                    let kind = if entry.filemode() == FILEMODE_SYMLINK {
                        FileKind::Symlink
                    } else {
                        FileKind::Regular
                    };
                    files.push(ListedFile {
                        path: format!("{}{}", root, name),
                        size: size as u64,
                        kind,
                    });
                }
            }
        }
        git2::TreeWalkResult::Ok
    })?;

    Ok(files)
}

/// Lists directories (Tree-type entries) depth-first, returning full
/// paths without a trailing slash (e.g. "foo", "foo/bar").
pub fn list_dirs_at_commit(
    _repo: &Repository,
    commit: &git2::Commit,
) -> Result<Vec<String>, Error> {
    let tree = commit.tree()?;
    let mut dirs = Vec::new();

    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Some(name) = entry.name() {
            if let Some(ObjectType::Tree) = entry.kind() {
                let full = format!("{}{}", root, name);
                dirs.push(full);
            }
        }
        git2::TreeWalkResult::Ok
    })?;

    Ok(dirs)
}

/// Stages the entire working tree (add -A equivalent) and creates a commit on
/// the given branch with the provided identity. Returns `Ok(None)` if there is
/// no difference between the working directory tree and HEAD's (nothing to commit).
pub fn commit_working_tree(
    repo: &Repository,
    branch: &str,
    user_name: &str,
    user_email: &str,
    message: &str,
) -> Result<Option<git2::Oid>, Error> {
    let mut index = repo.index()?;
    // Callback that skips nested `.git/` paths when the user pastes a complete
    // git repository inside the mount (e.g. `athena-docs/` with `.git/objects/...`).
    // Without the filter, libgit2 treats `athena-docs/` as a submodule with no
    // entry in `.gitmodules` and returns `invalid path: 'athena-docs/'; class=Index (10)`.
    // The whole commit failed in a loop, leaving the files untracked.
    // With the filter, the contents of `athena-docs/` (Dockerfile, _media/, etc.)
    // are committed normally; only the inner `.git/` subdir is left out.
    let mut filter = |path: &std::path::Path, _matched: &[u8]| -> i32 {
        let s = path.to_string_lossy();
        if s == ".git"
            || s.starts_with(".git/")
            || s.contains("/.git/")
            || s.ends_with("/.git")
        {
            1 // skip
        } else {
            0 // include
        }
    };
    index.add_all(
        ["*"].iter(),
        git2::IndexAddOption::DEFAULT,
        Some(&mut filter),
    )?;
    index.write()?;
    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;

    let parent_commit = repo.head()?.peel_to_commit().ok();
    if let Some(ref parent) = parent_commit {
        if parent.tree_id() == tree_id {
            return Ok(None);
        }
    }

    let sig = git2::Signature::now(user_name, user_email)?;
    let parents: Vec<&git2::Commit> = parent_commit.as_ref().map(|c| vec![c]).unwrap_or_default();
    let ref_name = format!("refs/heads/{}", branch);
    let oid = repo.commit(Some(&ref_name), &sig, &sig, message, &tree, &parents)?;
    Ok(Some(oid))
}

/// Network transport used by fetch and push.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// The system `git` binary. Authentication goes through OpenSSH (honors
    /// `~/.ssh/config`, the agent, the macOS Keychain and every key type) or
    /// the configured HTTPS credential helper — the same path a manual
    /// `git fetch` takes, so "works in the terminal" means "works in the daemon".
    Cli,
    /// In-process libgit2/libssh2 (fallback when no `git` binary exists).
    Libgit2,
}

/// Process-wide transport. Defaults to libgit2 (library and test default);
/// the mount mode resolves the configured value at boot via `set_transport`.
static TRANSPORT_CLI: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_transport(t: Transport) {
    TRANSPORT_CLI.store(t == Transport::Cli, std::sync::atomic::Ordering::SeqCst);
}

pub fn transport() -> Transport {
    if TRANSPORT_CLI.load(std::sync::atomic::Ordering::SeqCst) {
        Transport::Cli
    } else {
        Transport::Libgit2
    }
}

/// Resolves the `git_transport` config value: `cli`, `libgit2` or `auto`
/// (CLI when a working `git` binary is on PATH, libgit2 otherwise).
pub fn resolve_transport(pref: &str) -> Transport {
    match pref {
        "libgit2" => Transport::Libgit2,
        "cli" => Transport::Cli,
        _ => {
            let works = std::process::Command::new("git")
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if works {
                Transport::Cli
            } else {
                Transport::Libgit2
            }
        }
    }
}

/// On-disk SSH keys for daemons without a usable ssh-agent, in preference
/// order. Each entry is `(private, public if present)`.
fn discover_ssh_keys(home: &Path) -> Vec<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|name| home.join(".ssh").join(name))
        .filter(|private| private.exists())
        .map(|private| {
            let public = private.with_extension("pub");
            let public = public.exists().then_some(public);
            (private, public)
        })
        .collect()
}

/// Attempt tracker for the libgit2 credential callback. libgit2 calls the
/// callback AGAIN after every failed authentication on the same connection;
/// a stateless callback that always answers "agent" loops until the server's
/// login grace time drops the connection ("Failed getting response" after
/// ~120 s on GitHub) and never reaches the on-disk keys — the 2026-09/10
/// incident where both machines failed every fetch for weeks.
#[derive(Default)]
struct CredAttempts {
    token: bool,
    username: bool,
    agent: bool,
    next_key: usize,
    default: bool,
}

/// Credential callback shared by push and fetch on the libgit2 transport:
/// HTTPS token > ssh-agent > each on-disk SSH key > default, every method
/// tried at most once per connection.
fn credential_callback(
    state: &mut CredAttempts,
    token: Option<&str>,
    username_from_url: Option<&str>,
    allowed: git2::CredentialType,
) -> Result<git2::Cred, Error> {
    if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
        if let Some(t) = token {
            if !state.token {
                state.token = true;
                return git2::Cred::userpass_plaintext("x-access-token", t);
            }
        }
    }
    let username = username_from_url.unwrap_or("git");
    if allowed.contains(git2::CredentialType::USERNAME) && !state.username {
        state.username = true;
        return git2::Cred::username(username);
    }
    if allowed.contains(git2::CredentialType::SSH_KEY) {
        if !state.agent {
            state.agent = true;
            if std::env::var_os("SSH_AUTH_SOCK").is_some() {
                if let Ok(cred) = git2::Cred::ssh_key_from_agent(username) {
                    return Ok(cred);
                }
            }
        }
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let keys = discover_ssh_keys(&home);
        while state.next_key < keys.len() {
            let (private, public) = &keys[state.next_key];
            state.next_key += 1;
            if let Ok(cred) = git2::Cred::ssh_key(username, public.as_deref(), private, None) {
                return Ok(cred);
            }
        }
    }
    if allowed.contains(git2::CredentialType::DEFAULT) && !state.default {
        state.default = true;
        return git2::Cred::default();
    }
    Err(Error::from_str(
        "authentication failed: every credential method was rejected (agent, ~/.ssh keys, token)",
    ))
}

/// Libgit2 remote callbacks with the stateful credential strategy.
fn libgit2_callbacks<'a>(token: Option<&str>) -> git2::RemoteCallbacks<'a> {
    let mut callbacks = git2::RemoteCallbacks::new();
    let token_owned = token.map(|t| t.to_string());
    let mut attempts = CredAttempts::default();
    callbacks.credentials(move |_url, username_from_url, allowed| {
        credential_callback(&mut attempts, token_owned.as_deref(), username_from_url, allowed)
    });
    callbacks
}

/// A network git command run without any output for this long is killed:
/// a hung connection must not hold the repo lock (and stall every drain)
/// forever. Progress output (`--progress`) keeps long transfers alive.
const CLI_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Runs a network `git` command against `repo` (fetch/push). Never prompts:
/// no stdin, `GIT_TERMINAL_PROMPT=0` and an `ssh -o BatchMode=yes` default
/// (unless the user configured their own `core.sshCommand`/`GIT_SSH_COMMAND`).
/// `on_activity` fires on the calling thread whenever the child writes
/// progress. Returns stdout on success; the error carries the stderr tail.
fn run_git_net(
    repo: &Repository,
    args: &[&str],
    token: Option<&str>,
    on_activity: &dyn Fn(),
) -> Result<String, Error> {
    use std::io::Read;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    let mut cmd = std::process::Command::new("git");
    cmd.arg("--git-dir")
        .arg(repo.path())
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let user_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some()
        || std::env::var_os("GIT_SSH").is_some()
        || repo
            .config()
            .and_then(|c| c.get_string("core.sshCommand"))
            .is_ok();
    if !user_ssh {
        cmd.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=30 -o ServerAliveInterval=15 -o ServerAliveCountMax=4",
        );
    }
    if let Some(t) = token {
        // Via env (not argv): the token never shows up in `ps`.
        let basic = base64_encode(format!("x-access-token:{}", t).as_bytes());
        cmd.env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.extraHeader")
            .env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {}", basic));
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::from_str(&format!("could not run git: {}", e)))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let activity = Arc::new(AtomicU64::new(0));
    let activity_w = activity.clone();
    let out_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    all.extend_from_slice(&chunk[..n]);
                    activity_w.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        all
    });

    let mut seen = 0u64;
    let mut last_activity = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        let now_seen = activity.load(Ordering::SeqCst);
        if now_seen != seen {
            seen = now_seen;
            last_activity = std::time::Instant::now();
            on_activity();
        } else if last_activity.elapsed() >= CLI_IDLE_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = out_reader.join();
            let _ = err_reader.join();
            return Err(Error::from_str(&format!(
                "git {} timed out ({} s without progress)",
                args.first().copied().unwrap_or(""),
                CLI_IDLE_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let out = out_reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    let status = status.map_err(|e| Error::from_str(&format!("waiting for git: {}", e)))?;
    let stdout_s = String::from_utf8_lossy(&out).into_owned();
    if status.success() {
        return Ok(stdout_s);
    }
    Err(Error::from_str(&format!(
        "git {} failed ({}): {}",
        args.first().copied().unwrap_or(""),
        status,
        cli_error_tail(&stdout_s, &String::from_utf8_lossy(&err))
    )))
}

/// Last meaningful lines of a failed git command: progress noise
/// (`\r`-rewritten counters) dropped, porcelain rejection lines (`!`) kept.
fn cli_error_tail(stdout: &str, stderr: &str) -> String {
    let mut lines: Vec<String> = stderr
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| {
            !(l.starts_with("Enumerating")
                || l.starts_with("Counting")
                || l.starts_with("Compressing")
                || l.starts_with("Writing")
                || l.starts_with("Total")
                || l.starts_with("remote: Counting")
                || l.starts_with("remote: Compressing")
                || l.starts_with("remote: Total")
                || l.starts_with("Receiving")
                || l.starts_with("Resolving")
                || l.starts_with("Unpacking")
                || l.starts_with("Delta compression"))
        })
        .map(str::to_string)
        .collect();
    lines.extend(
        stdout
            .lines()
            .filter(|l| l.starts_with('!'))
            .map(str::to_string),
    );
    let start = lines.len().saturating_sub(6);
    lines[start..].join(" | ")
}

/// Standard base64 (RFC 4648, padded) — only for the HTTP Basic header.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Pushes one refspec through the CLI. A server-side rejection (non-FF,
/// GitHub's GH001 for oversized blobs, protected branch) makes `git push`
/// exit non-zero, so — unlike libgit2 — there is no silent "push ok".
fn push_refspec_cli(
    repo: &Repository,
    remote_name: &str,
    refspec: &str,
    token: Option<&str>,
    on_activity: &dyn Fn(),
) -> Result<(), Error> {
    run_git_net(
        repo,
        &["push", "--porcelain", "--progress", remote_name, refspec],
        token,
        on_activity,
    )
    .map(|_| ())
    .map_err(|e| {
        let m = e.message();
        // "[rejected]"/"[remote rejected]" porcelain lines and "! " refs mean
        // the server refused the ref update; anything else is transport.
        if m.contains("rejected") || m.contains("| !") || m.starts_with('!') {
            Error::from_str(&format!("push rejected by remote: {}", m))
        } else {
            Error::from_str(&format!("push failed: {}", m))
        }
    })
}

/// Pushes one refspec through libgit2, collecting server-side rejections from
/// the `push_update_reference` callback (git2 returns Ok at the transport
/// level even when the ref is rejected — e.g. GitHub's GH001; the
/// 2026-07-03 "push ok" incident).
fn push_refspec_libgit2(
    repo: &Repository,
    remote_name: &str,
    refspec: &str,
    token: Option<&str>,
    on_activity: &dyn Fn(),
) -> Result<(), Error> {
    let mut remote = repo.find_remote(remote_name)?;
    let mut callbacks = libgit2_callbacks(token);
    let rejections: std::rc::Rc<std::cell::RefCell<Vec<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let rejections_cb = rejections.clone();
    callbacks.push_update_reference(move |refname, status| {
        if let Some(msg) = status {
            rejections_cb
                .borrow_mut()
                .push(format!("{}: {}", refname, msg));
        }
        Ok(())
    });
    callbacks.push_transfer_progress(move |_current, _total, _bytes| {
        on_activity();
    });
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(callbacks);
    remote.push(&[refspec], Some(&mut push_opts))?;
    let rejections = rejections.borrow();
    if !rejections.is_empty() {
        return Err(Error::from_str(&format!(
            "push rejected by remote: {}",
            rejections.join("; ")
        )));
    }
    Ok(())
}

/// Pushes one refspec with the process-wide transport.
fn push_refspec(
    repo: &Repository,
    remote_name: &str,
    refspec: &str,
    token: Option<&str>,
    on_activity: &dyn Fn(),
) -> Result<(), Error> {
    match transport() {
        Transport::Cli => push_refspec_cli(repo, remote_name, refspec, token, on_activity),
        Transport::Libgit2 => push_refspec_libgit2(repo, remote_name, refspec, token, on_activity),
    }
}

/// Pushes `refs/heads/<branch>:refs/heads/<branch>` to the given remote.
/// If `token` is Some, uses HTTPS userpass authentication ("x-access-token").
/// Otherwise, delegates to the system (ssh-agent / credential helper).
pub fn push_to_remote(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
) -> Result<(), Error> {
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch);
    push_refspec(repo, remote_name, &refspec, token, &|| {})
}

/// Progress events emitted by `push_progressive`.
pub enum PushProgress {
    /// Pack bytes are flowing to the remote (raw transfer heartbeat).
    Transfer,
    /// One intermediate commit was accepted by the remote.
    CommitPushed { done: usize, total: usize },
}

/// Publishes every commit the remote is missing, ONE PUSH PER COMMIT
/// (`<sha>:refs/heads/<branch>`, oldest first along the first-parent chain).
///
/// `git push` packs EVERYTHING missing on the remote into a single pack — with
/// a mass copy that was a 24 GB pack, which GitHub aborts mid-transfer
/// (2026-07-10 incident). Pushing commit by commit bounds every pack to about
/// one drain batch, each step is a fast-forward by construction, and a failure
/// midway leaves the remote on an intermediate commit — the next cycle resumes
/// from exactly there (self-healing catch-up).
///
/// Returns the number of commits published (0 = already up to date).
/// `Err` when the remote tip is unknown locally (another machine moved the
/// branch — the caller reconciles with fetch + merge and retries).
pub fn push_progressive(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
    on_progress: &dyn Fn(PushProgress),
) -> Result<usize, Error> {
    let local_tip = repo.refname_to_id(&format!("refs/heads/{}", branch))?;

    // Remote tip via fetch + remote-tracking ref. (`Remote::list()` on an
    // EMPTY advertisement hits a null-pointer `from_raw_parts` inside git2
    // 0.18 — aborts under the strict UB checks.) A failed fetch (empty
    // remote, branch not born yet, offline) leaves the tip unknown and we
    // attempt a from-root catch-up: if the remote actually has history, the
    // first push is rejected as non-FF and lands in the reconcile path.
    let _ = fetch_remote(repo, branch, remote_name, token);
    let remote_tip = repo
        .refname_to_id(&format!("refs/remotes/{}/{}", remote_name, branch))
        .ok();

    if remote_tip == Some(local_tip) {
        return Ok(0);
    }
    if let Some(tip) = remote_tip {
        // The tip must be an ancestor — otherwise another machine moved the
        // branch and a plain push would be non-FF anyway: merge first.
        if !repo.graph_descendant_of(local_tip, tip)? {
            return Err(Error::from_str(
                "remote branch is not an ancestor of the local branch — merge required",
            ));
        }
    }

    // Pushable steps: the first-parent chain from the local tip down, KEEPING
    // ONLY commits that descend from the remote tip — each push is then a
    // fast-forward by construction. Below a merge that incorporated the
    // remote tip, the first-parent line continues into the OTHER lineage
    // (e.g. this machine's pre-merge history): those commits do NOT descend
    // from the remote tip, and pushing one would rewind the branch — the
    // 2026-07-10 NotFastForward on the Mac replica's convergence merge. They
    // ride inside the merge commit's own pack instead.
    let mut commits: Vec<git2::Oid> = Vec::new();
    let mut cur = local_tip;
    loop {
        if remote_tip == Some(cur) {
            break;
        }
        let descends = match remote_tip {
            Some(tip) => repo.graph_descendant_of(cur, tip)?,
            None => true, // empty remote: the whole root lineage fast-forwards
        };
        if !descends {
            break;
        }
        commits.push(cur);
        match repo.find_commit(cur)?.parent_id(0) {
            Ok(parent) => cur = parent,
            Err(_) => break, // root commit reached
        }
    }
    commits.reverse();
    let total = commits.len();

    for (i, oid) in commits.iter().enumerate() {
        let refspec = format!("{}:refs/heads/{}", oid, branch);
        push_refspec(repo, remote_name, &refspec, token, &|| {
            on_progress(PushProgress::Transfer)
        })
        .map_err(|e| {
            Error::from_str(&format!("at commit {}/{}: {}", i + 1, total, e.message()))
        })?;
        on_progress(PushProgress::CommitPushed {
            done: i + 1,
            total,
        });
    }
    Ok(total)
}

/// Runs `git fetch` against the given remote (only the required branch),
/// updating `refs/remotes/<remote>/<branch>`. Uses the process-wide transport.
/// A remote that does not have the branch yet (empty repo) is not an error.
pub fn fetch_remote(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
) -> Result<(), Error> {
    match transport() {
        Transport::Cli => fetch_remote_cli(repo, branch, remote_name, token),
        Transport::Libgit2 => fetch_remote_libgit2(repo, branch, remote_name, token),
    }
}

fn fetch_remote_cli(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
) -> Result<(), Error> {
    let refspec = format!("+refs/heads/{0}:refs/remotes/{1}/{0}", branch, remote_name);
    match run_git_net(
        repo,
        &["fetch", "--no-tags", "--progress", remote_name, &refspec],
        token,
        &|| {},
    ) {
        Ok(_) => Ok(()),
        // Empty remote / branch not born yet: nothing to fetch.
        Err(e) if e.message().contains("couldn't find remote ref") => Ok(()),
        Err(e) => Err(e),
    }
}

fn fetch_remote_libgit2(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
) -> Result<(), Error> {
    let mut remote = repo.find_remote(remote_name)?;
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(libgit2_callbacks(token));
    remote.fetch(&[branch], Some(&mut fetch_opts), None)
}

/// Attempts to fast-forward the local HEAD to `origin/<branch>`.
///
/// - `Ok(true)` if a fast-forward was applied (HEAD and working tree advanced).
/// - `Ok(false)` if the local side is already at the remote revision.
/// - `Err(_)` if the histories diverged (a real merge is needed) or on
///   I/O errors / missing reference.
pub fn merge_ff_only(repo: &Repository, branch: &str) -> Result<bool, Error> {
    let remote_ref_name = format!("refs/remotes/origin/{}", branch);
    let remote_ref = repo.find_reference(&remote_ref_name)?;
    let fetch_commit = repo.reference_to_annotated_commit(&remote_ref)?;
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_up_to_date() {
        return Ok(false);
    }
    if !analysis.is_fast_forward() {
        return Err(Error::from_str(
            "non-fast-forward merge: histories diverged, manual resolution required",
        ));
    }

    let local_ref_name = format!("refs/heads/{}", branch);
    let mut local_ref = repo.find_reference(&local_ref_name)?;
    let target_oid = fetch_commit.id();
    local_ref.set_target(target_oid, "git-drive: fast-forward")?;
    repo.set_head(&local_ref_name)?;
    repo.checkout_head(Some(
        git2::build::CheckoutBuilder::default().force(),
    ))?;
    Ok(true)
}

/// Outcome of a pull attempt (fetch + merge).
#[derive(Debug, Clone)]
pub enum MergeOutcome {
    /// The remote lacks the branch, or the local side is already at the same revision.
    NoOp,
    /// Fast-forward applied — HEAD and working tree advanced to `refs/remotes/.../<branch>`.
    FastForwarded,
    /// 3-way merge applied without conflicts, with a merge commit created.
    Merged,
    /// 3-way merge with conflicts. For each conflicted path, the local version
    /// was preserved as `<path>.<suffix>` and the remote version was accepted as canonical.
    Conflicted {
        /// Original paths that were in conflict (before resolution).
        resolved: Vec<String>,
        /// Suffix used for the conflicted copies (without the leading `.`).
        suffix: String,
    },
}

/// Merges `refs/remotes/<remote>/<branch>` into the local HEAD.
///
/// Behavior:
/// - If the remote lacks the branch or we are already at the same revision -> `NoOp`.
/// - If a fast-forward is possible -> applies it and returns `FastForwarded`.
/// - If the histories truly diverged -> attempts a 3-way merge.
///   - No conflicts -> creates a merge commit and returns `Merged`.
///   - With conflicts -> for each conflicted file, saves the local version
///     as `<path>.<conflict_suffix>`, accepts the remote version as canonical,
///     and creates a merge commit. Returns `Conflicted`.
///
/// Requires an identity (`user_name`/`user_email`) to create merge commits.
pub fn pull_and_merge(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    user_name: &str,
    user_email: &str,
    conflict_suffix: &str,
) -> Result<MergeOutcome, Error> {
    let remote_ref_name = format!("refs/remotes/{}/{}", remote_name, branch);
    let remote_ref = match repo.find_reference(&remote_ref_name) {
        Ok(r) => r,
        Err(_) => return Ok(MergeOutcome::NoOp),
    };
    let fetch_commit = repo.reference_to_annotated_commit(&remote_ref)?;
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_up_to_date() {
        return Ok(MergeOutcome::NoOp);
    }

    if analysis.is_fast_forward() {
        let local_ref_name = format!("refs/heads/{}", branch);
        let mut local_ref = repo.find_reference(&local_ref_name)?;
        local_ref.set_target(fetch_commit.id(), "git-drive: fast-forward")?;
        repo.set_head(&local_ref_name)?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
        return Ok(MergeOutcome::FastForwarded);
    }

    if !analysis.is_normal() {
        return Err(Error::from_str("merge analysis: unexpected state"));
    }

    repo.merge(&[&fetch_commit], None, None)?;
    let mut index = repo.index()?;
    let mut conflicted_paths: Vec<String> = Vec::new();

    if index.has_conflicts() {
        let workdir = repo
            .workdir()
            .ok_or_else(|| Error::from_str("repo has no workdir (bare?) - cannot merge"))?
            .to_path_buf();

        let conflicts: Vec<(Option<git2::IndexEntry>, Option<git2::IndexEntry>)> = {
            let iter = index.conflicts()?;
            iter.filter_map(|c| c.ok())
                .map(|c| (c.our, c.their))
                .collect()
        };

        for (our, their) in conflicts {
            let path_bytes = our
                .as_ref()
                .or(their.as_ref())
                .map(|e| e.path.clone())
                .ok_or_else(|| Error::from_str("conflict without an identifiable path"))?;
            let path_str = String::from_utf8(path_bytes)
                .map_err(|_| Error::from_str("path contains invalid bytes"))?;
            let full = workdir.join(&path_str);

            match (&our, &their) {
                // Both sides changed the file: the local version is preserved
                // as a conflicted copy and the remote one becomes canonical.
                (Some(our_entry), Some(their_entry)) => {
                    let our_blob = repo.find_blob(our_entry.id)?;
                    let conflict_name = format!("{}.{}", path_str, conflict_suffix);
                    write_workdir_file(&workdir.join(&conflict_name), our_blob.content())?;
                    index.add_path(Path::new(&conflict_name))?;
                    let their_blob = repo.find_blob(their_entry.id)?;
                    write_workdir_file(&full, their_blob.content())?;
                    index.add_path(Path::new(&path_str))?;
                    conflicted_paths.push(path_str);
                }
                // Modify/delete: only ONE version survives, so there is
                // nothing to "conflict" — the edit wins and stays at its own
                // path (Dropbox semantics). A conflicted copy here left the
                // original name gone and, in encrypted mode, an orphan copy
                // whose long-name sidecar the delete had removed (unreadable
                // literal name + EIO on the drive, 2026-10-13).
                (Some(kept), None) | (None, Some(kept)) => {
                    let blob = repo.find_blob(kept.id)?;
                    write_workdir_file(&full, blob.content())?;
                    index.add_path(Path::new(&path_str))?;
                    log::info!(
                        "merge: modify/delete on {} — the modified version was kept",
                        path_str
                    );
                }
                (None, None) => {}
            }
        }

        // Long-name sidecars (`dslong_<h>.name`) are separate files: the side
        // that deleted an entry also deleted its sidecar, cleanly. Restore the
        // sidecar of every surviving hashed entry so its name stays decodable.
        restore_missing_long_name_sidecars(repo, &mut index, &workdir, &fetch_commit.id())?;
        index.write()?;
    }

    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = git2::Signature::now(user_name, user_email)?;

    let head_commit = repo.head()?.peel_to_commit()?;
    let their_commit = repo.find_commit(fetch_commit.id())?;

    let message = if conflicted_paths.is_empty() {
        format!("git-drive: merge {}/{}", remote_name, branch)
    } else {
        format!(
            "git-drive: merge {}/{} with {} conflict(s) - local versions preserved as *.{}",
            remote_name,
            branch,
            conflicted_paths.len(),
            conflict_suffix
        )
    };

    let ref_name = format!("refs/heads/{}", branch);
    repo.commit(
        Some(&ref_name),
        &sig,
        &sig,
        &message,
        &tree,
        &[&head_commit, &their_commit],
    )?;

    repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
    repo.cleanup_state()?;

    if conflicted_paths.is_empty() {
        Ok(MergeOutcome::Merged)
    } else {
        Ok(MergeOutcome::Conflicted {
            resolved: conflicted_paths,
            suffix: conflict_suffix.to_string(),
        })
    }
}

/// Writes a file into the working tree, creating parent directories.
fn write_workdir_file(full: &Path, content: &[u8]) -> Result<(), Error> {
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::from_str(&format!("create_dir: {}", e)))?;
    }
    std::fs::write(full, content).map_err(|e| Error::from_str(&format!("write {:?}: {}", full, e)))
}

/// For every hashed long-name entry (`dslong_<h>[.conflict-...]`) in the
/// merged index whose sidecar is missing, restores the sidecar blob from the
/// local HEAD tree or the remote tree. Crypto-agnostic: sidecars are plain
/// files keyed by name.
fn restore_missing_long_name_sidecars(
    repo: &Repository,
    index: &mut git2::Index,
    workdir: &Path,
    their_oid: &git2::Oid,
) -> Result<(), Error> {
    let present: std::collections::HashSet<String> = index
        .iter()
        .filter_map(|e| String::from_utf8(e.path).ok())
        .collect();
    // Every hashed component along every path counts — a long DIRECTORY name
    // keeps its sidecar in the parent, and the side that deleted the whole
    // directory took that sidecar with it.
    let mut checked: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut missing: Vec<String> = Vec::new();
    for path in &present {
        let mut dir = String::new();
        for comp in path.split('/') {
            let here = if dir.is_empty() {
                comp.to_string()
            } else {
                format!("{}/{}", dir, comp)
            };
            if crate::crypt::is_long_name(comp) && checked.insert(here.clone()) {
                let with_dir = |n: String| {
                    if dir.is_empty() {
                        n
                    } else {
                        format!("{}/{}", dir, n)
                    }
                };
                let exact_p = with_dir(crate::crypt::sidecar_name(comp));
                let base_p = crate::crypt::long_name_base(comp)
                    .map(|b| with_dir(crate::crypt::sidecar_name(b)));
                let has = present.contains(&exact_p)
                    || base_p.as_ref().map(|b| present.contains(b)).unwrap_or(false);
                if !has {
                    missing.push(base_p.unwrap_or(exact_p));
                }
            }
            dir = here;
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let head_tree = repo.head()?.peel_to_commit()?.tree()?;
    let their_tree = repo.find_commit(*their_oid)?.tree()?;
    for sidecar in missing {
        let found = [&head_tree, &their_tree]
            .iter()
            .find_map(|t| t.get_path(Path::new(&sidecar)).ok().map(|e| e.id()));
        if let Some(id) = found {
            let blob = repo.find_blob(id)?;
            write_workdir_file(&workdir.join(&sidecar), blob.content())?;
            index.add_path(Path::new(&sidecar))?;
            log::info!("merge: restored long-name sidecar {}", sidecar);
        }
    }
    Ok(())
}

/// Timestamps for the paths touched by the commits in `old..new` (reachable
/// from `new`, not from `old`), first touch from the newest commit winning —
/// the incremental counterpart of `commit_times_by_path` for a post-pull
/// refresh. `Ok(None)` when the range exceeds `cap` commits (the caller does
/// the full walk instead).
pub fn commit_times_between(
    repo: &Repository,
    old: git2::Oid,
    new: git2::Oid,
    cap: usize,
) -> Result<Option<std::collections::HashMap<String, std::time::SystemTime>>, Error> {
    let mut out: std::collections::HashMap<String, std::time::SystemTime> =
        std::collections::HashMap::new();
    let mut revwalk = repo.revwalk()?;
    revwalk.push(new)?;
    revwalk.hide(old)?;
    revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
    for (n, oid) in revwalk.enumerate() {
        if n >= cap {
            return Ok(None);
        }
        let commit = repo.find_commit(oid?)?;
        let time = commit_system_time(&commit);
        let tree = commit.tree()?;
        let parent_tree = match commit.parent(0) {
            Ok(parent) => Some(parent.tree()?),
            Err(_) => None,
        };
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
        for delta in diff.deltas() {
            if let Some(path) = delta.new_file().path().and_then(|p| p.to_str()) {
                out.entry(path.to_string()).or_insert(time);
            }
        }
    }
    Ok(Some(out))
}

/// Converts a commit's committer timestamp into a `SystemTime` (UTC).
fn commit_system_time(commit: &git2::Commit) -> std::time::SystemTime {
    let secs = commit.time().seconds();
    if secs >= 0 {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)
    } else {
        std::time::UNIX_EPOCH
    }
}

/// Timestamp of the last commit that touched each path in HEAD, computed in
/// a SINGLE revwalk (not one walk per file). Walks backwards from HEAD,
/// diffing each commit against its first parent; the first (most recent)
/// commit that touches a path defines its timestamp. Stops at the
/// `depth_cap` depth limit or once every path is resolved; the remainder
/// receives the HEAD timestamp (cheap, deterministic fallback).
/// Merge commits are diffed only against the first parent — files brought in
/// by the merge receive the merge's timestamp (documented approximation).
pub fn commit_times_by_path(
    repo: &Repository,
    depth_cap: usize,
) -> Result<std::collections::HashMap<String, std::time::SystemTime>, Error> {
    use std::collections::{HashMap, HashSet};

    let head = repo.head()?.peel_to_commit()?;
    let head_time = commit_system_time(&head);
    let mut pending: HashSet<String> = list_files_at_commit(repo, &head)?
        .into_iter()
        .map(|f| f.path)
        .collect();
    let mut out: HashMap<String, std::time::SystemTime> = HashMap::new();

    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

    for (depth, oid) in revwalk.enumerate() {
        if pending.is_empty() || depth >= depth_cap {
            break;
        }
        let commit = repo.find_commit(oid?)?;
        let time = commit_system_time(&commit);
        let tree = commit.tree()?;
        let parent_tree = match commit.parent(0) {
            Ok(parent) => Some(parent.tree()?),
            Err(_) => None, // root commit: diff against the empty tree
        };
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
        for delta in diff.deltas() {
            if let Some(path) = delta.new_file().path().and_then(|p| p.to_str()) {
                if pending.remove(path) {
                    out.insert(path.to_string(), time);
                }
            }
        }
    }

    for path in pending {
        out.insert(path, head_time);
    }
    Ok(out)
}

/// Lists working tree files that match ignore rules (`.gitignore`,
/// `info/exclude`, `core.excludesfile`) and are NOT in the index. These are
/// the "visible local-only" candidates: they exist on disk and never enter a
/// commit. Symlinks come with `FileKind::Symlink` and size = target length.
pub fn list_ignored_files(repo: &Repository) -> Result<Vec<ListedFile>, Error> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| Error::from_str("repo has no workdir (bare?)"))?
        .to_path_buf();
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(true);
    let statuses = repo.statuses(Some(&mut opts))?;

    let mut out = Vec::new();
    for entry in statuses.iter() {
        if !entry.status().contains(git2::Status::IGNORED) {
            continue;
        }
        let Some(path) = entry.path() else { continue };
        let full = workdir.join(path);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            let size = std::fs::read_link(&full)
                .map(|t| t.as_os_str().len() as u64)
                .unwrap_or(0);
            out.push(ListedFile {
                path: path.to_string(),
                size,
                kind: FileKind::Symlink,
            });
        } else if meta.is_file() {
            out.push(ListedFile {
                path: path.to_string(),
                size: meta.len(),
                kind: FileKind::Regular,
            });
        }
        // Ignored directories are not included: their inner files already
        // arrive individually via recurse_ignored_dirs.
    }
    Ok(out)
}

/// `true` if the working tree has NO pending change at all (staged, unstaged
/// or untracked; ignored files are excluded). Pull watcher guard:
/// merge/checkout force may only run on a clean tree — otherwise uncommitted
/// local changes would be destroyed (RF6 of Phase 0).
pub fn is_working_tree_clean(repo: &Repository) -> Result<bool, Error> {
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    Ok(statuses.is_empty())
}

pub fn current_branch_name(repo: &Repository) -> Result<String, Error> {
    let head = repo.head()?;
    if !head.is_branch() {
        return Err(Error::from_str(
            "HEAD is detached - git-drive requires a named branch",
        ));
    }
    head.shorthand()
        .map(|s| s.to_string())
        .ok_or_else(|| Error::from_str("branch has no name (empty shorthand)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::tempdir;

    fn init_repo_with_commit() -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        {
            let sig = Signature::now("Test", "test@example.com").unwrap();
            let tree_id = {
                let mut index = repo.index().unwrap();
                index.write_tree().unwrap()
            };
            let tree = repo.find_tree(tree_id).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
        }
        (dir, repo)
    }

    #[test]
    fn current_branch_name_returns_default_branch_on_fresh_repo() {
        let (_dir, repo) = init_repo_with_commit();
        let name = current_branch_name(&repo).unwrap();
        assert!(
            name == "main" || name == "master",
            "expected main or master, got {}",
            name
        );
    }

    #[test]
    fn current_branch_name_after_checkout_new_branch() {
        let (_dir, repo) = init_repo_with_commit();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature/x", &head_commit, false).unwrap();
        repo.set_head("refs/heads/feature/x").unwrap();
        assert_eq!(current_branch_name(&repo).unwrap(), "feature/x");
    }

    #[test]
    fn current_branch_name_returns_err_on_detached_head() {
        let (_dir, repo) = init_repo_with_commit();
        let head_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.set_head_detached(head_commit.id()).unwrap();
        assert!(current_branch_name(&repo).is_err());
    }

    fn commit_file(repo: &Repository, relpath: &str, content: &[u8]) {
        let workdir = repo.workdir().unwrap();
        let full = workdir.join(relpath);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(relpath)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let sig = Signature::now("Test", "test@example.com").unwrap();
        let parent_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add", &tree, &[&parent_commit])
            .unwrap();
    }

    /// Creates a real symlink in the working tree and commits it (120000 entry).
    fn commit_symlink(repo: &Repository, relpath: &str, target: &str) {
        let workdir = repo.workdir().unwrap();
        let full = workdir.join(relpath);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::os::unix::fs::symlink(target, &full).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(relpath)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let sig = Signature::now("Test", "test@example.com").unwrap();
        let parent_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add symlink", &tree, &[&parent_commit])
            .unwrap();
    }

    #[test]
    fn list_files_at_commit_distinguishes_symlink_kind() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "regular.txt", b"data");
        commit_symlink(&repo, "link.txt", "regular.txt");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let files = list_files_at_commit(&repo, &commit).unwrap();

        let regular = files.iter().find(|f| f.path == "regular.txt").unwrap();
        let link = files.iter().find(|f| f.path == "link.txt").unwrap();
        assert_eq!(regular.kind, FileKind::Regular);
        assert_eq!(link.kind, FileKind::Symlink);
        // A symlink's blob is the target text.
        assert_eq!(link.size, "regular.txt".len() as u64);
    }

    #[test]
    fn list_files_at_commit_nested_symlink_kind() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "docs/real.md", b"conteudo");
        commit_symlink(&repo, "docs/alias.md", "real.md");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let files = list_files_at_commit(&repo, &commit).unwrap();
        let link = files.iter().find(|f| f.path == "docs/alias.md").unwrap();
        assert_eq!(link.kind, FileKind::Symlink);
    }

    #[test]
    fn list_files_at_commit_flat_repo() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"aa");
        commit_file(&repo, "b.txt", b"bbb");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let mut files: Vec<(String, u64)> = list_files_at_commit(&repo, &commit)
            .unwrap()
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![("a.txt".to_string(), 2), ("b.txt".to_string(), 3)]
        );
    }

    #[test]
    fn list_files_at_commit_nested() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        commit_file(&repo, "foo/b.txt", b"yy");
        commit_file(&repo, "foo/bar/c.txt", b"zzz");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let mut files: Vec<(String, u64)> = list_files_at_commit(&repo, &commit)
            .unwrap()
            .into_iter()
            .map(|f| (f.path, f.size))
            .collect();
        files.sort();
        assert_eq!(
            files,
            vec![
                ("a.txt".to_string(), 1),
                ("foo/b.txt".to_string(), 2),
                ("foo/bar/c.txt".to_string(), 3),
            ]
        );
    }

    #[test]
    fn list_files_at_commit_empty_tree() {
        let (_dir, repo) = init_repo_with_commit();
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let files = list_files_at_commit(&repo, &commit).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn list_dirs_at_commit_nested() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        commit_file(&repo, "foo/b.txt", b"y");
        commit_file(&repo, "foo/bar/c.txt", b"z");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let mut dirs = list_dirs_at_commit(&repo, &commit).unwrap();
        dirs.sort();
        assert_eq!(dirs, vec!["foo".to_string(), "foo/bar".to_string()]);
    }

    #[test]
    fn list_dirs_at_commit_empty_on_flat_repo() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        let commit = repo.head().unwrap().peel_to_commit().unwrap();
        let dirs = list_dirs_at_commit(&repo, &commit).unwrap();
        assert!(dirs.is_empty());
    }

    #[test]
    fn commit_working_tree_creates_commit() {
        let (dir, repo) = init_repo_with_commit();
        std::fs::write(dir.path().join("novo.txt"), b"hello").unwrap();

        let branch = current_branch_name(&repo).unwrap();
        let oid = commit_working_tree(
            &repo,
            &branch,
            "Alice",
            "alice@example.com",
            "manual add",
        )
        .unwrap()
        .expect("must return Some (there are changes)");

        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.id(), oid);
        assert_eq!(head.message().unwrap(), "manual add");
        assert_eq!(head.author().name().unwrap(), "Alice");
    }

    #[test]
    fn commit_working_tree_returns_none_when_clean() {
        let (_dir, repo) = init_repo_with_commit();
        let branch = current_branch_name(&repo).unwrap();
        let res = commit_working_tree(&repo, &branch, "Alice", "alice@example.com", "noop")
            .unwrap();
        assert!(res.is_none(), "expected None when there are no changes");
    }

    fn setup_local_with_bare_remote() -> (tempfile::TempDir, Repository, tempfile::TempDir) {
        let (dir, repo) = init_repo_with_commit();
        let bare = tempdir().unwrap();
        Repository::init_bare(bare.path()).unwrap();
        let url = format!("file://{}", bare.path().to_str().unwrap());
        repo.remote("origin", &url).unwrap();
        let branch = current_branch_name(&repo).unwrap();
        push_to_remote(&repo, &branch, "origin", None).unwrap();
        (dir, repo, bare)
    }

    #[test]
    fn push_progressive_publishes_each_missing_commit() {
        let (_local, repo, bare) = setup_local_with_bare_remote();
        let branch = current_branch_name(&repo).unwrap();
        commit_file(&repo, "a.txt", b"a");
        commit_file(&repo, "b.txt", b"b");
        commit_file(&repo, "c.txt", b"c");

        let events = std::cell::RefCell::new(Vec::new());
        let n = push_progressive(&repo, &branch, "origin", None, &|ev| {
            if let PushProgress::CommitPushed { done, total } = ev {
                events.borrow_mut().push((done, total));
            }
        })
        .unwrap();
        assert_eq!(n, 3, "three missing commits published one by one");
        assert_eq!(*events.borrow(), vec![(1, 3), (2, 3), (3, 3)]);

        // Remote tip reached the local tip.
        let bare_repo = Repository::open(bare.path()).unwrap();
        let remote_tip = bare_repo
            .refname_to_id(&format!("refs/heads/{}", branch))
            .unwrap();
        let local_tip = repo
            .refname_to_id(&format!("refs/heads/{}", branch))
            .unwrap();
        assert_eq!(remote_tip, local_tip);

        // Idempotent: nothing more to publish.
        let n2 = push_progressive(&repo, &branch, "origin", None, &|_| {}).unwrap();
        assert_eq!(n2, 0);
    }

    #[test]
    fn push_progressive_to_empty_remote_pushes_from_root() {
        let (_dir, repo) = init_repo_with_commit();
        let bare = tempdir().unwrap();
        Repository::init_bare(bare.path()).unwrap();
        let url = format!("file://{}", bare.path().to_str().unwrap());
        repo.remote("origin", &url).unwrap();
        let branch = current_branch_name(&repo).unwrap();
        commit_file(&repo, "x.txt", b"x");

        let n = push_progressive(&repo, &branch, "origin", None, &|_| {}).unwrap();
        assert_eq!(n, 2, "root commit + x.txt, one push each");
        let bare_repo = Repository::open(bare.path()).unwrap();
        assert_eq!(
            bare_repo
                .refname_to_id(&format!("refs/heads/{}", branch))
                .unwrap(),
            repo.refname_to_id(&format!("refs/heads/{}", branch))
                .unwrap()
        );
    }

    #[test]
    fn push_progressive_after_merge_pushes_only_descendants_of_remote_tip() {
        // Machine A and the remote share `base`; B moves the remote; A has its
        // own lineage, fetches and MERGES. The catch-up must start at the
        // merge commit (the only pushable step): the first-parent chain below
        // it runs into A's pre-merge lineage, which does NOT descend from the
        // remote tip — pushing it would rewind the branch (NotFastForward,
        // 2026-07-10 Mac replica incident).
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        let branch = current_branch_name(&repo_a).unwrap();

        // B pushes b1.
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"b");
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        // A diverges with two local commits, fetches and merges b1.
        commit_file(&repo_a, "a1.txt", b"1");
        commit_file(&repo_a, "a2.txt", b"2");
        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let remote_ref = repo_a
            .find_reference(&format!("refs/remotes/origin/{}", branch))
            .unwrap();
        let theirs = remote_ref.peel_to_commit().unwrap();
        let ours = repo_a.head().unwrap().peel_to_commit().unwrap();
        let mut index = repo_a
            .merge_commits(&ours, &theirs, None)
            .expect("3-way merge");
        assert!(!index.has_conflicts());
        let tree_id = index.write_tree_to(&repo_a).unwrap();
        let tree = repo_a.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Alice", "a@x.com").unwrap();
        repo_a
            .commit(Some("HEAD"), &sig, &sig, "merge", &tree, &[&ours, &theirs])
            .unwrap();

        // Only the merge commit is pushable — and it fast-forwards the remote
        // to the local tip in a single step.
        let events = std::cell::RefCell::new(Vec::new());
        let n = push_progressive(&repo_a, &branch, "origin", None, &|ev| {
            if let PushProgress::CommitPushed { done, total } = ev {
                events.borrow_mut().push((done, total));
            }
        })
        .unwrap();
        assert_eq!(n, 1, "the merge is the single pushable step");
        assert_eq!(*events.borrow(), vec![(1, 1)]);
        let bare_repo = Repository::open(bare.path()).unwrap();
        assert_eq!(
            bare_repo
                .refname_to_id(&format!("refs/heads/{}", branch))
                .unwrap(),
            repo_a
                .refname_to_id(&format!("refs/heads/{}", branch))
                .unwrap()
        );
    }

    #[test]
    fn push_progressive_errors_when_remote_moved_by_another_machine() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        // Machine B moves the remote branch without A knowing.
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"hi");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        // A has a local commit too; the remote tip is unknown to A.
        commit_file(&repo_a, "from_a.txt", b"oi");
        let err = push_progressive(&repo_a, &branch, "origin", None, &|_| {});
        assert!(err.is_err(), "unknown remote tip must trigger reconcile");
    }

    #[test]
    fn fetch_remote_brings_new_commits() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        // Another clone (B) makes a change and pushes
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"hello");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        // A fetches
        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let remote_ref = repo_a
            .find_reference(&format!("refs/remotes/origin/{}", branch))
            .unwrap();
        let remote_commit = remote_ref.peel_to_commit().unwrap();
        assert_eq!(remote_commit.message().unwrap().trim(), "add");
    }

    #[test]
    fn merge_ff_only_applies_fast_forward() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"hi");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let result = merge_ff_only(&repo_a, &branch).unwrap();
        assert!(result, "expected Ok(true) for an applied fast-forward");

        // A's working tree now has from_b.txt
        let workdir = repo_a.workdir().unwrap();
        assert!(workdir.join("from_b.txt").exists());
    }

    #[test]
    fn merge_ff_only_returns_false_when_up_to_date() {
        let (_local, repo, _bare) = setup_local_with_bare_remote();
        let branch = current_branch_name(&repo).unwrap();
        fetch_remote(&repo, &branch, "origin", None).unwrap();
        let result = merge_ff_only(&repo, &branch).unwrap();
        assert!(!result, "expected Ok(false) when already up to date");
    }

    #[test]
    fn merge_ff_only_returns_err_on_divergence() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();

        // A makes a divergent local commit (does not publish)
        commit_file(&repo_a, "local_only.txt", b"a");

        // B publishes a commit as well
        let dir_b = tempdir().unwrap();
        let repo_b = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir_b.path(),
        )
        .unwrap();
        {
            let mut cfg = repo_b.config().unwrap();
            cfg.set_str("user.name", "Bob").unwrap();
            cfg.set_str("user.email", "b@x.com").unwrap();
        }
        commit_file(&repo_b, "from_b.txt", b"b");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        assert!(
            merge_ff_only(&repo_a, &branch).is_err(),
            "expected an error on divergence"
        );
    }

    fn clone_of(bare: &tempfile::TempDir, author: &str) -> (tempfile::TempDir, Repository) {
        let dir = tempdir().unwrap();
        let repo = Repository::clone(
            &format!("file://{}", bare.path().to_str().unwrap()),
            dir.path(),
        )
        .unwrap();
        {
            let mut cfg = repo.config().unwrap();
            cfg.set_str("user.name", author).unwrap();
            cfg.set_str("user.email", &format!("{}@x.com", author)).unwrap();
        }
        (dir, repo)
    }

    #[test]
    fn pull_and_merge_noop_when_remote_branch_missing() {
        let (_dir, repo) = init_repo_with_commit();
        let branch = current_branch_name(&repo).unwrap();
        let outcome =
            pull_and_merge(&repo, &branch, "origin", "A", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::NoOp));
    }

    #[test]
    fn pull_and_merge_noop_when_up_to_date() {
        let (_local, repo, _bare) = setup_local_with_bare_remote();
        let branch = current_branch_name(&repo).unwrap();
        fetch_remote(&repo, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo, &branch, "origin", "A", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::NoOp));
    }

    #[test]
    fn pull_and_merge_fast_forwards_when_only_remote_advanced() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        commit_file(&repo_b, "from_b.txt", b"hi");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo_a, &branch, "origin", "A", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::FastForwarded));
        assert!(repo_a.workdir().unwrap().join("from_b.txt").exists());
    }

    #[test]
    fn pull_and_merge_3way_merges_when_different_files_touched() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        {
            let mut cfg = repo_a.config().unwrap();
            cfg.set_str("user.name", "Alice").unwrap();
            cfg.set_str("user.email", "a@x.com").unwrap();
        }
        commit_file(&repo_a, "only_a.txt", b"from A");

        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        commit_file(&repo_b, "only_b.txt", b"from B");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo_a, &branch, "origin", "Alice", "a@x.com", "conflict-test").unwrap();
        assert!(matches!(outcome, MergeOutcome::Merged), "expected Merged, got {:?}", outcome);

        let wd = repo_a.workdir().unwrap();
        assert!(wd.join("only_a.txt").exists(), "A's local file must persist");
        assert!(wd.join("only_b.txt").exists(), "B's remote file must arrive");

        let head = repo_a.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2, "merge commit must have 2 parents");
    }

    fn delete_and_commit(repo: &Repository, relpath: &str) {
        let workdir = repo.workdir().unwrap();
        let _ = std::fs::remove_file(workdir.join(relpath));
        let mut index = repo.index().unwrap();
        index.remove_path(std::path::Path::new(relpath)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = Signature::now("Test", "test@example.com").unwrap();
        let parent_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "rm", &tree, &[&parent_commit])
            .unwrap();
    }

    /// A and B share `base`; A applies `local`, B applies `remote` and pushes;
    /// A fetches and merges. Returns A's repo (and keeps the temp dirs alive).
    fn diverge_and_merge(
        base: &[(&str, &[u8])],
        local: &dyn Fn(&Repository),
        remote: &dyn Fn(&Repository),
    ) -> (Vec<tempfile::TempDir>, Repository, MergeOutcome) {
        let (local_a, repo_a, bare) = setup_local_with_bare_remote();
        let branch = current_branch_name(&repo_a).unwrap();
        for (p, c) in base {
            commit_file(&repo_a, p, c);
        }
        push_to_remote(&repo_a, &branch, "origin", None).unwrap();
        let (dir_b, repo_b) = clone_of(&bare, "Bob");
        remote(&repo_b);
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();
        local(&repo_a);
        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo_a, &branch, "origin", "Alice", "a@x.com", "conflict-xyz").unwrap();
        (vec![local_a, bare, dir_b], repo_a, outcome)
    }

    #[test]
    fn pull_and_merge_modify_vs_remote_delete_keeps_the_edit() {
        let (_keep, repo_a, outcome) = diverge_and_merge(
            &[("doc.txt", b"base")],
            &|a| commit_file(a, "doc.txt", b"edited locally"),
            &|b| delete_and_commit(b, "doc.txt"),
        );
        assert!(matches!(outcome, MergeOutcome::Merged), "got {:?}", outcome);
        let wd = repo_a.workdir().unwrap();
        assert_eq!(std::fs::read(wd.join("doc.txt")).unwrap(), b"edited locally");
        assert!(!wd.join("doc.txt.conflict-xyz").exists(), "no conflicted copy for modify/delete");
        let head = repo_a.head().unwrap().peel_to_commit().unwrap();
        assert!(head.tree().unwrap().get_path(Path::new("doc.txt")).is_ok());
        assert!(repo_a.state() == git2::RepositoryState::Clean);
    }

    #[test]
    fn pull_and_merge_local_delete_vs_remote_modify_keeps_the_edit() {
        let (_keep, repo_a, outcome) = diverge_and_merge(
            &[("doc.txt", b"base")],
            &|a| delete_and_commit(a, "doc.txt"),
            &|b| commit_file(b, "doc.txt", b"edited remotely"),
        );
        assert!(matches!(outcome, MergeOutcome::Merged), "got {:?}", outcome);
        let wd = repo_a.workdir().unwrap();
        assert_eq!(std::fs::read(wd.join("doc.txt")).unwrap(), b"edited remotely");
    }

    #[test]
    fn pull_and_merge_restores_long_name_sidecar_deleted_by_the_other_side() {
        // Encrypted layout: hashed entry + sidecar. B deletes both (as the
        // drain does); A edits the entry. The kept edit needs its sidecar.
        let (_keep, repo_a, _outcome) = diverge_and_merge(
            &[("d/dslong_abc", b"sealed v1"), ("d/dslong_abc.name", b"ENCNAME")],
            &|a| commit_file(a, "d/dslong_abc", b"sealed v2"),
            &|b| {
                delete_and_commit(b, "d/dslong_abc");
                delete_and_commit(b, "d/dslong_abc.name");
            },
        );
        let wd = repo_a.workdir().unwrap();
        assert_eq!(std::fs::read(wd.join("d/dslong_abc")).unwrap(), b"sealed v2");
        assert_eq!(std::fs::read(wd.join("d/dslong_abc.name")).unwrap(), b"ENCNAME");
        let tree = repo_a.head().unwrap().peel_to_commit().unwrap().tree().unwrap();
        assert!(tree.get_path(Path::new("d/dslong_abc.name")).is_ok(), "sidecar committed");
    }

    #[test]
    fn pull_and_merge_restores_sidecar_of_long_directory_deleted_remotely() {
        let (_keep, repo_a, _outcome) = diverge_and_merge(
            &[("dslong_dir/file", b"v1"), ("dslong_dir.name", b"ENCDIR")],
            &|a| commit_file(a, "dslong_dir/file", b"v2"),
            &|b| {
                delete_and_commit(b, "dslong_dir/file");
                delete_and_commit(b, "dslong_dir.name");
            },
        );
        let wd = repo_a.workdir().unwrap();
        assert_eq!(std::fs::read(wd.join("dslong_dir/file")).unwrap(), b"v2");
        assert_eq!(std::fs::read(wd.join("dslong_dir.name")).unwrap(), b"ENCDIR");
    }

    #[test]
    fn pull_and_merge_conflict_creates_conflict_copy_and_accepts_remote() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        {
            let mut cfg = repo_a.config().unwrap();
            cfg.set_str("user.name", "Alice").unwrap();
            cfg.set_str("user.email", "a@x.com").unwrap();
        }
        // The file "shared.txt" exists on both sides with different content
        commit_file(&repo_a, "shared.txt", b"local version");

        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        commit_file(&repo_b, "shared.txt", b"remote version");
        let branch = current_branch_name(&repo_b).unwrap();
        push_to_remote(&repo_b, &branch, "origin", None).unwrap();

        fetch_remote(&repo_a, &branch, "origin", None).unwrap();
        let outcome =
            pull_and_merge(&repo_a, &branch, "origin", "Alice", "a@x.com", "conflict-xyz").unwrap();

        match outcome {
            MergeOutcome::Conflicted { resolved, suffix } => {
                assert_eq!(resolved, vec!["shared.txt".to_string()]);
                assert_eq!(suffix, "conflict-xyz");
            }
            other => panic!("expected Conflicted, got {:?}", other),
        }

        let wd = repo_a.workdir().unwrap();
        let canonical = std::fs::read(wd.join("shared.txt")).unwrap();
        assert_eq!(canonical, b"remote version", "canonical must be the remote version");
        let conflict_copy = std::fs::read(wd.join("shared.txt.conflict-xyz")).unwrap();
        assert_eq!(conflict_copy, b"local version", "conflict copy must hold the local version");

        let head = repo_a.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(head.parent_count(), 2, "conflict merge commit must have 2 parents");
        // Merge state must be clean (no dangling MERGE_HEAD)
        assert!(repo_a.state() == git2::RepositoryState::Clean);
    }

    #[test]
    fn is_working_tree_clean_true_on_fresh_commit() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        assert!(is_working_tree_clean(&repo).unwrap());
    }

    #[test]
    fn is_working_tree_clean_false_on_modified_tracked_file() {
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, "a.txt", b"x");
        std::fs::write(dir.path().join("a.txt"), b"editado").unwrap();
        assert!(!is_working_tree_clean(&repo).unwrap());
    }

    #[test]
    fn is_working_tree_clean_false_on_untracked_file() {
        let (dir, repo) = init_repo_with_commit();
        std::fs::write(dir.path().join("novo.txt"), b"untracked").unwrap();
        assert!(!is_working_tree_clean(&repo).unwrap());
    }

    #[test]
    fn discover_ssh_keys_lists_existing_keys_in_preference_order() {
        let dir = tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        assert!(discover_ssh_keys(dir.path()).is_empty());

        std::fs::write(ssh.join("id_rsa"), b"k").unwrap();
        let keys = discover_ssh_keys(dir.path());
        assert_eq!(keys.len(), 1);
        assert!(keys[0].0.ends_with(".ssh/id_rsa"));
        assert!(keys[0].1.is_none());

        std::fs::write(ssh.join("id_ed25519"), b"k").unwrap();
        std::fs::write(ssh.join("id_ed25519.pub"), b"p").unwrap();
        let keys = discover_ssh_keys(dir.path());
        assert_eq!(keys.len(), 2, "every key is a candidate, not only the first");
        assert!(keys[0].0.ends_with(".ssh/id_ed25519"), "ed25519 takes precedence");
        assert!(keys[0].1.as_ref().unwrap().ends_with("id_ed25519.pub"));
        assert!(keys[1].0.ends_with(".ssh/id_rsa"));
    }

    #[test]
    fn credential_callback_never_repeats_a_method() {
        // libgit2 re-invokes the callback after each rejected attempt; the
        // stateless version answered "agent" forever (weeks of failed fetches).
        let mut st = CredAttempts::default();
        let allowed = git2::CredentialType::SSH_KEY;
        let mut produced = 0;
        while credential_callback(&mut st, None, Some("git"), allowed).is_ok() {
            produced += 1;
            assert!(produced < 10, "the callback must run out of methods");
        }
        assert!(st.agent, "the agent was attempted (or skipped) exactly once");

        // Token: offered once for HTTPS, then exhausted.
        let mut st = CredAttempts::default();
        let https = git2::CredentialType::USER_PASS_PLAINTEXT;
        assert!(credential_callback(&mut st, Some("t"), None, https).is_ok());
        assert!(credential_callback(&mut st, Some("t"), None, https).is_err());
    }

    #[test]
    fn cli_fetch_and_push_roundtrip_via_bare_remote() {
        let (_local_a, repo_a, bare) = setup_local_with_bare_remote();
        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        let branch = current_branch_name(&repo_b).unwrap();
        commit_file(&repo_b, "from_b.txt", b"hello");
        let tip_b = repo_b.head().unwrap().peel_to_commit().unwrap().id();
        let refspec = format!("{}:refs/heads/{}", tip_b, branch);
        push_refspec_cli(&repo_b, "origin", &refspec, None, &|| {}).unwrap();

        fetch_remote_cli(&repo_a, &branch, "origin", None).unwrap();
        let fetched = repo_a
            .find_reference(&format!("refs/remotes/origin/{}", branch))
            .unwrap()
            .peel_to_commit()
            .unwrap();
        assert_eq!(fetched.id(), tip_b, "libgit2 sees the objects/refs the CLI wrote");
    }

    #[test]
    fn cli_fetch_from_empty_remote_is_not_an_error() {
        let (_dir, repo) = init_repo_with_commit();
        let bare = tempdir().unwrap();
        Repository::init_bare(bare.path()).unwrap();
        repo.remote("origin", &format!("file://{}", bare.path().to_str().unwrap()))
            .unwrap();
        let branch = current_branch_name(&repo).unwrap();
        fetch_remote_cli(&repo, &branch, "origin", None).unwrap();
    }

    #[test]
    fn cli_push_reports_server_side_rejection() {
        // Same GH001 reproduction as the libgit2 test: a locked ref on the
        // remote makes the ref update fail — git push must return Err.
        let (_dir, repo, bare) = setup_local_with_bare_remote();
        commit_file(&repo, "novo.txt", b"x");
        let branch = current_branch_name(&repo).unwrap();
        let lock = bare.path().join(format!("refs/heads/{}.lock", branch));
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, b"").unwrap();
        let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch);
        let err = push_refspec_cli(&repo, "origin", &refspec, None, &|| {}).unwrap_err();
        assert!(err.message().contains("push rejected"), "got: {}", err.message());
    }

    #[test]
    fn cli_push_non_fast_forward_is_rejected() {
        let (_dir_a, repo_a, bare) = setup_local_with_bare_remote();
        let (_dir_b, repo_b) = clone_of(&bare, "Bob");
        let branch = current_branch_name(&repo_b).unwrap();
        commit_file(&repo_b, "b.txt", b"b");
        let spec = format!("refs/heads/{0}:refs/heads/{0}", branch);
        push_refspec_cli(&repo_b, "origin", &spec, None, &|| {}).unwrap();
        commit_file(&repo_a, "a.txt", b"a");
        assert!(push_refspec_cli(&repo_a, "origin", &spec, None, &|| {}).is_err());
    }

    #[test]
    fn commit_times_between_covers_only_the_new_range() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file(&repo, "old.txt", b"1");
        let old = repo.head().unwrap().peel_to_commit().unwrap().id();
        commit_file(&repo, "new.txt", b"2");
        commit_file(&repo, "old.txt", b"3");
        let new = repo.head().unwrap().peel_to_commit().unwrap().id();
        let changed = commit_times_between(&repo, old, new, 100).unwrap().unwrap();
        let mut keys: Vec<_> = changed.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, vec!["new.txt".to_string(), "old.txt".to_string()]);
        assert!(commit_times_between(&repo, old, new, 1).unwrap().is_none(), "cap -> full walk");
        assert!(commit_times_between(&repo, new, new, 100).unwrap().unwrap().is_empty());
    }

    #[test]
    fn base64_encode_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn cli_error_tail_drops_progress_and_keeps_rejections() {
        let stderr = "Enumerating objects: 3, done.\rCounting objects: 100% (3/3)\nerror: failed to push some refs\n";
        let stdout = "To /tmp/x\n!\trefs/heads/main:refs/heads/main\t[rejected] (non-fast-forward)\nDone\n";
        let tail = cli_error_tail(stdout, stderr);
        assert!(tail.contains("failed to push"));
        assert!(tail.contains("[rejected]"));
        assert!(!tail.contains("Counting"));
    }

    // ---- Phase 2.5, Block A: commit policy and ignored files ----

    #[test]
    fn is_working_tree_clean_true_with_only_ignored_files() {
        // Behavior pin: ignored files do not block the pull guard.
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.log\n");
        std::fs::write(dir.path().join("app.log"), b"ignorado").unwrap();
        assert!(
            is_working_tree_clean(&repo).unwrap(),
            "an ignored file must not dirty the working tree for the pull guard"
        );
    }

    #[test]
    fn commit_working_tree_skips_ignored_file() {
        // Behavior pin: IndexAddOption::DEFAULT honors the ignore rules —
        // previously a side effect, now specified.
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.log\n");
        std::fs::write(dir.path().join("app.log"), b"ignorado").unwrap();
        std::fs::write(dir.path().join("real.txt"), b"real").unwrap();

        let branch = current_branch_name(&repo).unwrap();
        let oid = commit_working_tree(&repo, &branch, "A", "a@x.com", "batch")
            .unwrap()
            .expect("real.txt must produce a commit");
        let commit = repo.find_commit(oid).unwrap();
        let tree = commit.tree().unwrap();
        assert!(tree.get_path(Path::new("real.txt")).is_ok());
        assert!(
            tree.get_path(Path::new("app.log")).is_err(),
            "app.log is ignored and must not enter the commit"
        );
    }

    #[test]
    fn list_ignored_files_reports_untracked_ignored_with_sizes() {
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.log\nbuild/\n");
        std::fs::write(dir.path().join("app.log"), b"12345").unwrap();
        std::fs::create_dir_all(dir.path().join("build")).unwrap();
        std::fs::write(dir.path().join("build/out.bin"), b"123").unwrap();
        // A non-ignored untracked file stays out
        std::fs::write(dir.path().join("real.txt"), b"x").unwrap();

        let mut files = list_ignored_files(&repo).unwrap();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let paths: Vec<(&str, u64)> = files.iter().map(|f| (f.path.as_str(), f.size)).collect();
        assert_eq!(
            paths,
            vec![("app.log", 5), ("build/out.bin", 3)],
            "only untracked ignored files, with on-disk sizes"
        );
        assert!(files.iter().all(|f| f.kind == FileKind::Regular));
    }

    #[test]
    fn list_ignored_files_reports_symlink_kind() {
        let (dir, repo) = init_repo_with_commit();
        commit_file(&repo, ".gitignore", b"*.lnk\n");
        std::os::unix::fs::symlink("alvo.txt", dir.path().join("a.lnk")).unwrap();
        let files = list_ignored_files(&repo).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "a.lnk");
        assert_eq!(files[0].kind, FileKind::Symlink);
        assert_eq!(files[0].size, "alvo.txt".len() as u64);
    }

    // ---- Phase 2.5, Block C: mtime derived from history ----

    /// Commits a file with an explicit committer timestamp (epoch seconds).
    fn commit_file_at(repo: &Repository, relpath: &str, content: &[u8], epoch_secs: i64) {
        let workdir = repo.workdir().unwrap();
        let full = workdir.join(relpath);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();

        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(relpath)).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();

        let time = git2::Time::new(epoch_secs, 0);
        let sig = Signature::new("Test", "test@example.com", &time).unwrap();
        let parent_commit = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "add", &tree, &[&parent_commit])
            .unwrap();
    }

    fn epoch(secs: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    #[test]
    fn commit_times_by_path_assigns_last_touch_per_path() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file_at(&repo, "a.txt", b"v1", 1_600_000_000);
        commit_file_at(&repo, "b.txt", b"y", 1_650_000_000);
        commit_file_at(&repo, "a.txt", b"v2", 1_700_000_000);

        let times = commit_times_by_path(&repo, 100).unwrap();
        assert_eq!(times.get("a.txt"), Some(&epoch(1_700_000_000)));
        assert_eq!(times.get("b.txt"), Some(&epoch(1_650_000_000)));
    }

    #[test]
    fn commit_times_by_path_depth_cap_falls_back_to_head_time() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file_at(&repo, "antigo.txt", b"x", 1_600_000_000);
        commit_file_at(&repo, "novo.txt", b"y", 1_700_000_000);

        // Cap 1: only the HEAD commit is diffed; antigo.txt falls back to the
        // HEAD timestamp, never to boot_time/an error.
        let times = commit_times_by_path(&repo, 1).unwrap();
        assert_eq!(times.get("novo.txt"), Some(&epoch(1_700_000_000)));
        assert_eq!(
            times.get("antigo.txt"),
            Some(&epoch(1_700_000_000)),
            "paths beyond the cap receive the HEAD timestamp"
        );
    }

    #[test]
    fn commit_times_by_path_covers_nested_paths() {
        let (_dir, repo) = init_repo_with_commit();
        commit_file_at(&repo, "docs/a.md", b"x", 1_620_000_000);
        let times = commit_times_by_path(&repo, 100).unwrap();
        assert_eq!(times.get("docs/a.md"), Some(&epoch(1_620_000_000)));
    }

    #[test]
    fn push_to_remote_detects_server_side_ref_rejection() {
        // Reproduces GitHub's GH001 (file > 100 MB): the server accepts the
        // connection but rejects the REF update. git2 returns Ok at the
        // transport level — the rejection only shows up in the
        // push_update_reference callback. Without handling it, the drive
        // logged "push ok" with the remote stalled (incident from the
        // 2026-07-03 validation).
        let (_dir, repo, bare) = setup_local_with_bare_remote();
        commit_file(&repo, "novo.txt", b"x");
        let branch = current_branch_name(&repo).unwrap();

        // Lock the ref in the bare repo: the ref update fails server-side.
        let lock = bare
            .path()
            .join(format!("refs/heads/{}.lock", branch));
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, b"").unwrap();

        let result = push_to_remote(&repo, &branch, "origin", None);
        assert!(
            result.is_err(),
            "a push with a server-rejected ref must return Err"
        );

        // And the remote indeed did not advance.
        let bare_repo = Repository::open(bare.path()).unwrap();
        let bare_head = bare_repo
            .find_reference(&format!("refs/heads/{}", branch))
            .unwrap()
            .peel_to_commit()
            .unwrap();
        let local_head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_ne!(bare_head.id(), local_head.id(), "the remote ref must not have advanced");
    }

    #[test]
    fn push_to_remote_via_bare_remote_succeeds() {
        let (dir, repo) = init_repo_with_commit();
        let bare = tempdir().unwrap();
        Repository::init_bare(bare.path()).unwrap();
        let url = format!("file://{}", bare.path().to_str().unwrap());
        repo.remote("origin", &url).unwrap();

        commit_file(&repo, "a.txt", b"hello");
        let branch = current_branch_name(&repo).unwrap();
        push_to_remote(&repo, &branch, "origin", None).unwrap();

        let bare_repo = Repository::open(bare.path()).unwrap();
        let bare_ref = bare_repo
            .find_reference(&format!("refs/heads/{}", branch))
            .unwrap();
        let bare_head = bare_ref.peel_to_commit().unwrap();
        let local_head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(bare_head.id(), local_head.id());
        drop(dir);
    }
}