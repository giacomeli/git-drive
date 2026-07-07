#![allow(dead_code)]

use git2::{Repository, Signature};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

pub struct TestRepo {
    pub dir: TempDir,
    pub repo: Repository,
}

pub fn init_repo() -> TestRepo {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = Repository::init(dir.path()).expect("git init");

    // Deterministic initial branch: without this, the name comes from the
    // environment (local init.defaultBranch = "main"; CI runners = "master")
    // and tests that reference "main" break outside the dev's machine.
    repo.set_head("refs/heads/main").expect("set_head main");

    {
        let mut config = repo.config().expect("repo config");
        config.set_str("user.name", "Test User").expect("set user.name");
        config.set_str("user.email", "test@example.com").expect("set user.email");
    }

    {
        let sig = Signature::now("Test User", "test@example.com").expect("signature");
        let tree_id = {
            let mut index = repo.index().expect("index");
            index.write_tree().expect("write empty tree")
        };
        let tree = repo.find_tree(tree_id).expect("find tree");
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .expect("initial commit");
    }

    TestRepo { dir, repo }
}

pub fn init_bare_remote() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir bare");
    let repo = Repository::init_bare(dir.path()).expect("git init --bare");
    // Same rationale as init_repo: without a deterministic HEAD, a clone of
    // this bare repo starts on the environment's default branch (master on
    // CI) and push/pull flows that reference "main" break outside the dev's
    // machine.
    repo.set_head("refs/heads/main").expect("set_head main");
    dir
}

pub fn add_remote(repo: &Repository, name: &str, url: &Path) {
    let url_str = url.to_str().expect("remote url utf8");
    repo.remote(name, url_str).expect("add remote");
}

pub fn commit_file(repo: &Repository, relpath: &str, content: &[u8], msg: &str) -> git2::Oid {
    let workdir = repo.workdir().expect("workdir");
    let full = workdir.join(relpath);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    fs::write(&full, content).expect("write file");

    let mut index = repo.index().expect("index");
    index.add_path(Path::new(relpath)).expect("index add");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");

    let sig = Signature::now("Test User", "test@example.com").expect("signature");
    let parent = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.as_ref().map(|c| vec![c]).unwrap_or_default();

    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
        .expect("commit")
}

/// Creates a real symlink in the working tree and commits it (120000 tree entry).
pub fn commit_symlink(repo: &Repository, relpath: &str, target: &str, msg: &str) -> git2::Oid {
    let workdir = repo.workdir().expect("workdir");
    let full = workdir.join(relpath);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    std::os::unix::fs::symlink(target, &full).expect("symlink");

    let mut index = repo.index().expect("index");
    index.add_path(Path::new(relpath)).expect("index add");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");

    let sig = Signature::now("Test User", "test@example.com").expect("signature");
    let parent = repo
        .head()
        .ok()
        .and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.as_ref().map(|c| vec![c]).unwrap_or_default();

    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
        .expect("commit")
}

/// Commits a file with an explicit committer timestamp (epoch seconds).
pub fn commit_file_at(
    repo: &Repository,
    relpath: &str,
    content: &[u8],
    epoch_secs: i64,
    msg: &str,
) -> git2::Oid {
    let workdir = repo.workdir().expect("workdir");
    let full = workdir.join(relpath);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("mkdir -p");
    }
    fs::write(&full, content).expect("write file");

    let mut index = repo.index().expect("index");
    index.add_path(Path::new(relpath)).expect("index add");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("write tree");
    let tree = repo.find_tree(tree_id).expect("find tree");

    let time = git2::Time::new(epoch_secs, 0);
    let sig = Signature::new("Test User", "test@example.com", &time).expect("signature");
    let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
    let parents: Vec<&git2::Commit> = parent.as_ref().map(|c| vec![c]).unwrap_or_default();

    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
        .expect("commit")
}

pub fn read_file(dir: &Path, relpath: &str) -> Vec<u8> {
    fs::read(dir.join(relpath)).expect("read file")
}

pub fn file_exists(dir: &Path, relpath: &str) -> bool {
    dir.join(relpath).exists()
}
