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
    let mut files = Vec::new();

    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Some(name) = entry.name() {
            if let Some(ObjectType::Blob) = entry.kind() {
                if let Ok(obj) = entry.to_object(repo) {
                    if let Some(blob) = obj.as_blob() {
                        let kind = if entry.filemode() == FILEMODE_SYMLINK {
                            FileKind::Symlink
                        } else {
                            FileKind::Regular
                        };
                        files.push(ListedFile {
                            path: format!("{}{}", root, name),
                            size: blob.size() as u64,
                            kind,
                        });
                    }
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

/// On-disk SSH key for daemons without an ssh-agent (systemd/launchd have no
/// SSH_AUTH_SOCK). Preference: id_ed25519, then id_rsa.
fn discover_ssh_key(home: &Path) -> Option<(std::path::PathBuf, Option<std::path::PathBuf>)> {
    for name in ["id_ed25519", "id_rsa"] {
        let private = home.join(".ssh").join(name);
        if private.exists() {
            let public = private.with_extension("pub");
            return Some((private.clone(), public.exists().then_some(public)));
        }
    }
    None
}

/// Credential callback shared by push and fetch: HTTPS token >
/// ssh-agent > on-disk SSH key > default.
fn credential_callback(
    token: Option<&str>,
    username_from_url: Option<&str>,
) -> Result<git2::Cred, Error> {
    if let Some(t) = token {
        return git2::Cred::userpass_plaintext("x-access-token", t);
    }
    let Some(username) = username_from_url else {
        return git2::Cred::default();
    };
    git2::Cred::ssh_key_from_agent(username)
        .or_else(|agent_err| {
            let home = std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_default();
            match discover_ssh_key(&home) {
                Some((private, public)) => {
                    git2::Cred::ssh_key(username, public.as_deref(), &private, None)
                }
                None => Err(agent_err),
            }
        })
        .or_else(|_| git2::Cred::default())
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
    let mut remote = repo.find_remote(remote_name)?;
    let mut callbacks = git2::RemoteCallbacks::new();
    let token_owned = token.map(|t| t.to_string());
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        credential_callback(token_owned.as_deref(), username_from_url)
    });
    // git2 returns Ok at the transport level even when the server REJECTS the
    // ref update (e.g. GitHub's GH001 for blobs > 100 MB) — the rejection only
    // arrives through the push_update_reference callback. Without collecting
    // it, the drain logged "push ok" with the remote stalled and the reconcile
    // retry never fired (incident from the 2026-07-03 validation).
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
    let mut push_opts = git2::PushOptions::new();
    push_opts.remote_callbacks(callbacks);
    let refspec = format!("refs/heads/{0}:refs/heads/{0}", branch);
    remote.push(&[&refspec], Some(&mut push_opts))?;
    let rejections = rejections.borrow();
    if !rejections.is_empty() {
        return Err(Error::from_str(&format!(
            "push rejected by remote: {}",
            rejections.join("; ")
        )));
    }
    Ok(())
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

    // Missing commits, oldest first, along the first-parent chain (merge
    // commits ride with their first parent; the second parent's history is
    // already on the remote or comes inside that single pack).
    let mut walk = repo.revwalk()?;
    walk.push(local_tip)?;
    if let Some(tip) = remote_tip {
        walk.hide(tip)?;
    }
    walk.simplify_first_parent()?;
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
    let commits: Vec<git2::Oid> = walk.collect::<Result<_, _>>()?;
    let total = commits.len();

    for (i, oid) in commits.iter().enumerate() {
        let mut remote = repo.find_remote(remote_name)?;
        let mut callbacks = git2::RemoteCallbacks::new();
        let token_owned = token.map(|t| t.to_string());
        callbacks.credentials(move |_url, username_from_url, _allowed| {
            credential_callback(token_owned.as_deref(), username_from_url)
        });
        // Same server-side rejection collection as `push_to_remote` (GH001 &
        // friends only surface through this callback, never as Err).
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
        let transfer_cb = on_progress;
        callbacks.push_transfer_progress(move |_current, _total, _bytes| {
            transfer_cb(PushProgress::Transfer);
        });
        let mut push_opts = git2::PushOptions::new();
        push_opts.remote_callbacks(callbacks);
        let refspec = format!("{}:refs/heads/{}", oid, branch);
        remote.push(&[&refspec], Some(&mut push_opts))?;
        let rejected = rejections.borrow();
        if !rejected.is_empty() {
            return Err(Error::from_str(&format!(
                "push rejected by remote at commit {}/{}: {}",
                i + 1,
                total,
                rejected.join("; ")
            )));
        }
        drop(rejected);
        on_progress(PushProgress::CommitPushed {
            done: i + 1,
            total,
        });
    }
    Ok(total)
}

/// Runs `git fetch` against the given remote (only the required branch).
/// Reuses the same credential strategy as `push_to_remote`.
pub fn fetch_remote(
    repo: &Repository,
    branch: &str,
    remote_name: &str,
    token: Option<&str>,
) -> Result<(), Error> {
    let mut remote = repo.find_remote(remote_name)?;
    let mut callbacks = git2::RemoteCallbacks::new();
    let token_owned = token.map(|t| t.to_string());
    callbacks.credentials(move |_url, username_from_url, _allowed| {
        credential_callback(token_owned.as_deref(), username_from_url)
    });
    let mut fetch_opts = git2::FetchOptions::new();
    fetch_opts.remote_callbacks(callbacks);
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
    local_ref.set_target(target_oid, "drive-sync: fast-forward")?;
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
        local_ref.set_target(fetch_commit.id(), "drive-sync: fast-forward")?;
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

            if let Some(our_entry) = &our {
                let our_blob = repo.find_blob(our_entry.id)?;
                let conflict_name = format!("{}.{}", path_str, conflict_suffix);
                let full_conflict = workdir.join(&conflict_name);
                if let Some(parent) = full_conflict.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::from_str(&format!("create_dir: {}", e)))?;
                }
                std::fs::write(&full_conflict, our_blob.content())
                    .map_err(|e| Error::from_str(&format!("write conflict copy: {}", e)))?;
                index.add_path(Path::new(&conflict_name))?;
            }

            let full = workdir.join(&path_str);
            if let Some(their_entry) = &their {
                let their_blob = repo.find_blob(their_entry.id)?;
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| Error::from_str(&format!("create_dir: {}", e)))?;
                }
                std::fs::write(&full, their_blob.content())
                    .map_err(|e| Error::from_str(&format!("write their: {}", e)))?;
                index.add_path(Path::new(&path_str))?;
            } else {
                let _ = std::fs::remove_file(&full);
                index.remove_path(Path::new(&path_str))?;
            }

            conflicted_paths.push(path_str);
        }

        index.write()?;
    }

    let tree_id = index.write_tree()?;
    let tree = repo.find_tree(tree_id)?;
    let sig = git2::Signature::now(user_name, user_email)?;

    let head_commit = repo.head()?.peel_to_commit()?;
    let their_commit = repo.find_commit(fetch_commit.id())?;

    let message = if conflicted_paths.is_empty() {
        format!("drive-sync: merge {}/{}", remote_name, branch)
    } else {
        format!(
            "drive-sync: merge {}/{} with {} conflict(s) - local versions preserved as *.{}",
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

    let mut depth = 0usize;
    for oid in revwalk {
        if pending.is_empty() || depth >= depth_cap {
            break;
        }
        depth += 1;
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
            "HEAD is detached - drive-sync requires a named branch",
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
    fn discover_ssh_key_prefers_ed25519_and_detects_pub() {
        let dir = tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        assert!(discover_ssh_key(dir.path()).is_none());

        std::fs::write(ssh.join("id_rsa"), b"k").unwrap();
        let (private, public) = discover_ssh_key(dir.path()).unwrap();
        assert!(private.ends_with(".ssh/id_rsa"));
        assert!(public.is_none());

        std::fs::write(ssh.join("id_ed25519"), b"k").unwrap();
        std::fs::write(ssh.join("id_ed25519.pub"), b"p").unwrap();
        let (private, public) = discover_ssh_key(dir.path()).unwrap();
        assert!(private.ends_with(".ssh/id_ed25519"), "ed25519 takes precedence");
        assert!(public.unwrap().ends_with("id_ed25519.pub"));
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