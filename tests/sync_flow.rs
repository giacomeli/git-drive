mod common;

use common::{commit_file, file_exists, init_bare_remote, init_repo, read_file};
use git_drive::sync::SyncManager;
use git2::Repository;
use std::time::Duration;

fn mgr(repo_path: &str, branch: &str, user: Option<(&str, &str)>) -> SyncManager {
    let (name, email) = match user {
        Some((n, e)) => (Some(n.to_string()), Some(e.to_string())),
        None => (None, None),
    };
    SyncManager::new_with_identity(repo_path, branch.to_string(), 1, name, email)
}

fn mgr_with_remote(
    repo_path: &str,
    branch: &str,
    user: Option<(&str, &str)>,
) -> SyncManager {
    let (name, email) = match user {
        Some((n, e)) => (Some(n.to_string()), Some(e.to_string())),
        None => (None, None),
    };
    SyncManager::new_full(
        repo_path,
        branch.to_string(),
        1,
        name,
        email,
        Some("origin".to_string()),
        None,
    )
}

async fn wait_flush() {
    tokio::time::sleep(Duration::from_millis(2500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_writes_file_and_commits() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_write("a.txt", b"hello".to_vec());
    wait_flush().await;

    assert_eq!(read_file(test.dir.path(), "a.txt"), b"hello");
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let msg = head.message().unwrap();
    assert!(msg.starts_with("git-drive:"), "msg={}", msg);
    assert!(msg.contains("a.txt"), "msg={}", msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_mkdir_creates_gitkeep() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_mkdir("foo");
    wait_flush().await;

    assert!(file_exists(test.dir.path(), "foo/.gitkeep"));
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(tree.get_path(std::path::Path::new("foo/.gitkeep")).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_rmdir_removes_dir() {
    let test = init_repo();
    // Setup: dir exists before the SyncManager starts
    std::fs::create_dir(test.dir.path().join("foo")).unwrap();
    std::fs::write(test.dir.path().join("foo/.gitkeep"), b"").unwrap();
    commit_file(&test.repo, "foo/.gitkeep", b"", "bootstrap foo");

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_rmdir("foo");
    wait_flush().await;

    assert!(!file_exists(test.dir.path(), "foo"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_rename_moves_file() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_rename("a.txt", "b.txt");
    wait_flush().await;

    assert!(!file_exists(test.dir.path(), "a.txt"));
    assert_eq!(read_file(test.dir.path(), "b.txt"), b"hello");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_no_user_identity_skips_commit() {
    let test = init_repo();
    let head_before = test.repo.head().unwrap().peel_to_commit().unwrap().id();

    let sync = mgr(test.dir.path().to_str().unwrap(), "main", None);
    sync.queue_write("a.txt", b"x".to_vec());
    wait_flush().await;

    assert!(file_exists(test.dir.path(), "a.txt"));
    let head_after = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        head_before, head_after,
        "no commit should have been created"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_multiple_ops_in_one_batch_produce_single_commit() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_write("a.txt", b"1".to_vec());
    sync.queue_write("b.txt", b"2".to_vec());
    sync.queue_write("c.txt", b"3".to_vec());
    wait_flush().await;

    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let parent = head.parent(0).unwrap();
    // parent must be the initial commit (a single new commit)
    assert_eq!(parent.parents().count(), 0);
    let msg = head.message().unwrap();
    assert!(msg.contains("3 changes"), "msg={}", msg);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_pushes_after_commit() {
    let test = init_repo();
    let bare = init_bare_remote();
    test.repo
        .remote(
            "origin",
            &format!("file://{}", bare.path().to_str().unwrap()),
        )
        .unwrap();

    let sync = mgr_with_remote(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_write("remote_me.txt", b"hi".to_vec());
    wait_flush().await;

    let bare_repo = Repository::open(bare.path()).unwrap();
    let bare_head = bare_repo
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    assert!(bare_head.message().unwrap().contains("remote_me.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pull_watcher_raises_refresh_flag_after_remote_change() {
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    // publish the initial commit to the remote
    git_drive::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    // Start the SyncManager with a low pull_interval
    let sync = SyncManager::new_full_with_pull(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        60,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
        Some("origin".to_string()),
        None,
        1,
    );

    // Another clone (B) pushes a new commit
    let dir_b = tempfile::tempdir().unwrap();
    let repo_b = git2::Repository::clone(&bare_url, dir_b.path()).unwrap();
    {
        let mut cfg = repo_b.config().unwrap();
        cfg.set_str("user.name", "Bob").unwrap();
        cfg.set_str("user.email", "b@x.com").unwrap();
    }
    std::fs::write(dir_b.path().join("from_b.txt"), b"hi").unwrap();
    let mut index = repo_b.index().unwrap();
    index.add_path(std::path::Path::new("from_b.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo_b.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Bob", "b@x.com").unwrap();
    let parent = repo_b.head().unwrap().peel_to_commit().unwrap();
    repo_b
        .commit(Some("HEAD"), &sig, &sig, "from B", &tree, &[&parent])
        .unwrap();
    git_drive::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    // wait for at least 2 pull cycles (1s each)
    tokio::time::sleep(Duration::from_millis(3500)).await;

    assert!(sync.take_refresh_flag(), "refresh_flag was not raised");
    // the file must be in A's working tree after the fast-forward
    assert!(file_exists(test.dir.path(), "from_b.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn refresh_if_needed_rebuilds_index_after_pull() {
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    git_drive::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    let sync = git_drive::sync::SyncManager::new_full_with_pull(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        60,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
        Some("origin".to_string()),
        None,
        1,
    );
    let mut fs = git_drive::fs::GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();
    assert!(fs.find_inode("from_b.txt").is_none());

    let dir_b = tempfile::tempdir().unwrap();
    let repo_b = git2::Repository::clone(&bare_url, dir_b.path()).unwrap();
    {
        let mut cfg = repo_b.config().unwrap();
        cfg.set_str("user.name", "Bob").unwrap();
        cfg.set_str("user.email", "b@x.com").unwrap();
    }
    std::fs::write(dir_b.path().join("from_b.txt"), b"hi").unwrap();
    let mut index = repo_b.index().unwrap();
    index.add_path(std::path::Path::new("from_b.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo_b.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Bob", "b@x.com").unwrap();
    let parent = repo_b.head().unwrap().peel_to_commit().unwrap();
    repo_b
        .commit(Some("HEAD"), &sig, &sig, "from B", &tree, &[&parent])
        .unwrap();
    git_drive::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    tokio::time::sleep(Duration::from_millis(3500)).await;

    fs.refresh_if_needed();
    assert!(fs.find_inode("from_b.txt").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_delete_nonexistent_sb_path_is_idempotent() {
    // Regression for the Finder paste bug:
    // the `op_touches_ephemeral` filter blocks Write/Mkdir on `.sb-*` paths
    // (TextEdit/NSDocument safe-save) but lets Delete/Rmdir through, so the
    // typical Finder sequence enqueues removals of paths that never reached
    // the disk. Before the fix, this became an ENOENT `eprintln!` in the log
    // ("disk: error removing ..."). Now it must be silent.
    let test = init_repo();
    let head_before = test.repo.head().unwrap().peel_to_commit().unwrap().id();

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );

    sync.queue_delete("hi.txt.sb-deadbeef-XXXXXX/hi.txt");
    sync.queue_rmdir("hi.txt.sb-deadbeef-XXXXXX");
    wait_flush().await;

    // Nothing changed in the working tree, so there must be no new commit.
    let head_after = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        head_before, head_after,
        "no commit should have been created"
    );

    // Buffer was drained (ops were processed, even if NotFound).
    assert!(
        sync.buffer_snapshot().is_empty(),
        "buffer should be empty after drain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_delete_existing_file_still_commits() {
    // Ensures the "silent NotFound" fix did not break the happy path:
    // deleting a file that exists still produces a normal commit.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_delete("a.txt");
    wait_flush().await;

    assert!(!file_exists(test.dir.path(), "a.txt"));
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    assert!(
        head.message().unwrap().contains("a.txt"),
        "commit should mention a.txt"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_push_failure_does_not_rollback_commit() {
    let test = init_repo();
    // invalid remote — push will fail
    test.repo
        .remote("origin", "file:///nonexistent/path/to/bare")
        .unwrap();

    let sync = mgr_with_remote(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_write("x.txt", b"y".to_vec());
    wait_flush().await;

    // the local commit must have been created even though the push failed
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    assert!(head.message().unwrap().contains("x.txt"));
}

// ---- RF3 regression (Phase 0): write buffer invalidation ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn clean_buffers_dropped_after_local_commit_dirty_survives() {
    // "Clean" buffers (content already enqueued and committed) must be
    // dropped when commit_generation advances; dirty buffers (pending
    // write without flush) must survive intact.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"old", "add a");
    commit_file(&test.repo, "b.txt", b"old-b", "add b");

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let mut fs = git_drive::fs::GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();
    let ino_a = fs.find_inode("a.txt").unwrap();
    let ino_b = fs.find_inode("b.txt").unwrap();

    // a.txt: full write + flush (proceeds to drain + commit)
    fs.apply_write(ino_a, 0, b"new").unwrap();
    fs.apply_flush(ino_a).expect("flush of a.txt should enqueue");
    // b.txt: pending write without flush (dirty)
    fs.apply_write(ino_b, 0, b"pending").unwrap();

    wait_flush().await; // drain applies + commits (commit_generation advances)

    fs.refresh_if_needed();

    assert_eq!(
        fs.buffered_count_for_test(),
        1,
        "only the dirty buffer (b.txt) should survive the gc"
    );
    assert_eq!(
        fs.read_content_for_test(ino_a).as_deref(),
        Some(b"new".as_slice()),
        "a.txt should come from the committed HEAD"
    );
    assert_eq!(
        fs.read_content_for_test(ino_b).as_deref(),
        Some(b"pending".as_slice()),
        "b.txt should still come from the dirty buffer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn remote_update_visible_after_refresh_despite_local_buffer() {
    // Headline scenario of the RF3 bug: after a local write, the buffer stayed
    // alive forever and reads never saw remote changes to that file again
    // (the buffer takes precedence over HEAD).
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    commit_file(&test.repo, "hi.txt", b"local-v1", "add hi");
    git_drive::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    let sync = SyncManager::new_full_with_pull(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        1,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
        Some("origin".to_string()),
        None,
        1,
    );
    let mut fs = git_drive::fs::GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();
    let ino = fs.find_inode("hi.txt").unwrap();

    // Local write: v2 (creates buffer) + flush + drain + commit + push
    fs.apply_write(ino, 0, b"local-v2").unwrap();
    fs.apply_flush(ino).expect("flush should enqueue");
    wait_flush().await;

    // B clones (already at v2), writes v3, and publishes
    let dir_b = tempfile::tempdir().unwrap();
    let repo_b = git2::Repository::clone(&bare_url, dir_b.path()).unwrap();
    {
        let mut cfg = repo_b.config().unwrap();
        cfg.set_str("user.name", "Bob").unwrap();
        cfg.set_str("user.email", "b@x.com").unwrap();
    }
    std::fs::write(dir_b.path().join("hi.txt"), b"remote-v3").unwrap();
    let mut index = repo_b.index().unwrap();
    index.add_path(std::path::Path::new("hi.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo_b.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Bob", "b@x.com").unwrap();
    let parent = repo_b.head().unwrap().peel_to_commit().unwrap();
    repo_b
        .commit(Some("HEAD"), &sig, &sig, "v3 from B", &tree, &[&parent])
        .unwrap();
    git_drive::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    // wait for the pull watcher to bring in the FF
    tokio::time::sleep(Duration::from_millis(3500)).await;

    fs.refresh_if_needed();
    assert_eq!(
        fs.read_content_for_test(ino).as_deref(),
        Some(b"remote-v3".as_slice()),
        "read should reflect the remote version after the refresh, not the stale buffer"
    );
}

// ---- RF5 regression (Phase 0): batch with causal ordering ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn batch_chained_dir_renames_apply_in_order() {
    // Previously (HashMap + sort by type), two renames in the same batch ran
    // in arbitrary order — one failed with ENOENT and the disk diverged from the index.
    let test = init_repo();
    commit_file(&test.repo, "d1/f.txt", b"x", "add d1");
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_rename("d1", "d2");
    sync.queue_rename("d2", "d3");
    wait_flush().await;

    assert!(!file_exists(test.dir.path(), "d1"), "d1 should not be left behind");
    assert!(!file_exists(test.dir.path(), "d2"), "d2 should not be left behind");
    assert_eq!(read_file(test.dir.path(), "d3/f.txt"), b"x");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn batch_rename_roundtrip_restores_original() {
    let test = init_repo();
    commit_file(&test.repo, "d1/f.txt", b"x", "add d1");
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_rename("d1", "d2");
    sync.queue_rename("d2", "d1");
    wait_flush().await;

    assert!(!file_exists(test.dir.path(), "d2"), "d2 should not be left behind");
    assert_eq!(read_file(test.dir.path(), "d1/f.txt"), b"x");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn batch_mkdir_rename_write_into_renamed_dir() {
    // Common Finder flow: create folder -> rename -> drop a file inside,
    // all within the debounce window. Previously, the Write (lower priority
    // than Rename) created the destination via create_dir_all and fs::rename
    // failed with "destination exists and is not empty" — the old dir came back.
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_mkdir("na");
    sync.queue_rename("na", "nb");
    sync.queue_write("nb/f.txt", b"conteudo".to_vec());
    wait_flush().await;

    assert!(!file_exists(test.dir.path(), "na"), "original dir should not be left behind");
    assert_eq!(read_file(test.dir.path(), "nb/f.txt"), b"conteudo");
    assert!(
        file_exists(test.dir.path(), "nb/.gitkeep"),
        "the mkdir's .gitkeep should have traveled along with the rename"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn batch_create_write_delete_leaves_no_residue() {
    let test = init_repo();
    let head_before = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_write("tmp.txt", b"efemero".to_vec());
    sync.queue_delete("tmp.txt");
    wait_flush().await;

    assert!(!file_exists(test.dir.path(), "tmp.txt"));
    let head_after = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        head_before, head_after,
        "a net-unchanged working tree should not produce a commit"
    );
}

// ---- RF6 regression (Phase 0): pull only with a clean working tree ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pull_watcher_skips_when_working_tree_dirty() {
    // Data loss scenario: in the identity-less mode, local writes live only
    // in the working tree (commits are skipped). Before the fix, the pull's
    // fast-forward ran checkout force and DESTROYED those changes.
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    commit_file(&test.repo, "hi.txt", b"v1", "add hi");
    git_drive::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    // Local change NOT committed (dirty working tree)
    std::fs::write(test.dir.path().join("hi.txt"), b"local-edit").unwrap();

    // Pull watcher without identity (degraded mode, FF-only), 1s interval
    let sync = SyncManager::new_full_with_pull(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        60,
        None,
        None,
        Some("origin".to_string()),
        None,
        1,
    );

    // B publishes v2 to the remote
    let dir_b = tempfile::tempdir().unwrap();
    let repo_b = git2::Repository::clone(&bare_url, dir_b.path()).unwrap();
    {
        let mut cfg = repo_b.config().unwrap();
        cfg.set_str("user.name", "Bob").unwrap();
        cfg.set_str("user.email", "b@x.com").unwrap();
    }
    std::fs::write(dir_b.path().join("hi.txt"), b"v2").unwrap();
    let mut index = repo_b.index().unwrap();
    index.add_path(std::path::Path::new("hi.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo_b.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Bob", "b@x.com").unwrap();
    let parent = repo_b.head().unwrap().peel_to_commit().unwrap();
    repo_b
        .commit(Some("HEAD"), &sig, &sig, "v2 from B", &tree, &[&parent])
        .unwrap();
    git_drive::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    // wait for more than one pull cycle
    tokio::time::sleep(Duration::from_millis(3500)).await;

    assert_eq!(
        read_file(test.dir.path(), "hi.txt"),
        b"local-edit",
        "an uncommitted local change must not be destroyed by the pull"
    );
    assert!(
        !sync.take_refresh_flag(),
        "pull should not have applied anything with a dirty working tree"
    );
}

// ---- Phase 1 RF4: daemon status.json ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn status_file_updated_after_drain() {
    let test = init_repo();
    let status_dir = tempfile::tempdir().unwrap();
    let status_path = status_dir.path().join("status.json");

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.enable_status_file(status_path.clone());
    assert!(status_path.exists(), "enable should write the initial state");

    sync.queue_write("a.txt", b"hello".to_vec());
    wait_flush().await;

    let status = git_drive::status::read_from(&status_path).unwrap();
    assert_eq!(status.branch, "main");
    assert_eq!(status.pending_ops, 0);
    let drain = status.last_drain.expect("drain should have been recorded");
    assert_eq!(drain.ops, 1);
    assert!(drain.commit.is_some(), "commit should have an oid");
    assert!(drain.pushed.is_none(), "with no remote configured, pushed is None");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn status_file_records_pull_outcome() {
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    git_drive::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    let status_dir = tempfile::tempdir().unwrap();
    let status_path = status_dir.path().join("status.json");

    let sync = SyncManager::new_full_with_pull(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        60,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
        Some("origin".to_string()),
        None,
        1,
    );
    sync.enable_status_file(status_path.clone());

    // wait for at least one pull cycle (nothing new on the remote)
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let status = git_drive::status::read_from(&status_path).unwrap();
    let pull = status.last_pull.expect("pull should have been recorded");
    assert_eq!(pull.outcome, "up to date");
}

// ---- Phase 2 RF2: drain_now on shutdown ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn drain_now_flushes_pending_ops_without_waiting_debounce() {
    // Clean shutdown: enqueued ops become a commit immediately, without
    // waiting for the debounce (deliberately high here).
    let test = init_repo();
    let sync = SyncManager::new_with_identity(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        3600, // 1h debounce — the watcher would never fire during the test
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
    );
    sync.queue_write("pendente.txt", b"nao pode se perder".to_vec());
    sync.queue_mkdir("dir-pendente");

    sync.drain_now();

    assert!(!sync.has_pending_ops(), "queue should be empty after drain_now");
    assert_eq!(read_file(test.dir.path(), "pendente.txt"), b"nao pode se perder");
    assert!(file_exists(test.dir.path(), "dir-pendente/.gitkeep"));
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    assert!(head.message().unwrap().contains("2 changes"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn drain_now_with_empty_queue_is_noop() {
    let test = init_repo();
    let head_before = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.drain_now();
    let head_after = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(head_before, head_after);
}

// ---- Phase 2.5, Block B: symlinks through drain and commit ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_symlink_materializes_and_commits_as_120000() {
    let test = init_repo();
    commit_file(&test.repo, "real.txt", b"data", "add real");
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.queue_symlink("alias.txt", "real.txt");
    wait_flush().await;

    let link_path = test.dir.path().join("alias.txt");
    let meta = std::fs::symlink_metadata(&link_path).expect("symlink should exist on disk");
    assert!(meta.file_type().is_symlink(), "should be a real symlink in the working tree");
    assert_eq!(
        std::fs::read_link(&link_path).unwrap(),
        std::path::PathBuf::from("real.txt")
    );

    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    let entry = tree.get_path(std::path::Path::new("alias.txt")).unwrap();
    assert_eq!(entry.filemode(), 0o120000, "tree entry should be a symlink (120000)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn symlink_roundtrip_created_in_fs_read_back_by_new_mount() {
    use git_drive::fs::GitDriveFS;
    use fuser::FileType;

    let test = init_repo();
    commit_file(&test.repo, "real.txt", b"data", "add real");
    let repo_path = test.dir.path().to_str().unwrap();
    let sync = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs = GitDriveFS::new(repo_path, "main".to_string(), sync).expect("fs");

    let (ino, attr) = fs.apply_symlink(1, "alias.txt", "real.txt").expect("apply_symlink");
    assert_eq!(attr.kind, FileType::Symlink);
    assert_eq!(fs.apply_readlink(ino).unwrap(), b"real.txt".to_vec());
    wait_flush().await;

    // A new mount reads the symlink back from HEAD
    let sync2 = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs2 = GitDriveFS::new(repo_path, "main".to_string(), sync2).expect("fs2");
    let ino2 = fs2.find_inode("alias.txt").expect("alias in the new mount's index");
    assert_eq!(fs2.apply_getattr(ino2).unwrap().kind, FileType::Symlink);
    assert_eq!(fs2.apply_readlink(ino2).unwrap(), b"real.txt".to_vec());
}

// ---- Phase 2.5, Block A: local-only ignored files through drain/commit ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ignored_file_stays_visible_and_out_of_commits() {
    use git_drive::fs::GitDriveFS;

    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"*.log\n", "add gitignore");
    let repo_path = test.dir.path().to_str().unwrap();
    let sync = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs = GitDriveFS::new(repo_path, "main".to_string(), sync).expect("fs");

    let (lino, _) = fs.apply_create(1, "app.log").unwrap();
    fs.apply_write(lino, 0, b"segredo local").unwrap();
    fs.apply_flush(lino);
    let (rino, _) = fs.apply_create(1, "real.txt").unwrap();
    fs.apply_write(rino, 0, b"publico").unwrap();
    fs.apply_flush(rino);
    wait_flush().await;

    // The commit happened (real.txt) and does NOT contain the ignored file
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(tree.get_path(std::path::Path::new("real.txt")).is_ok());
    assert!(
        tree.get_path(std::path::Path::new("app.log")).is_err(),
        "app.log is ignored and must not appear in a commit"
    );
    // The working tree persists the local-only file
    assert_eq!(read_file(test.dir.path(), "app.log"), b"segredo local");

    // Post-gc (commit_generation advanced), still visible and readable
    fs.refresh_if_needed();
    let ino = fs.find_inode("app.log").expect("visible after other files were committed");
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"segredo local".to_vec());

    // A remount re-adopts it from the working tree
    let sync2 = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs2 = GitDriveFS::new(repo_path, "main".to_string(), sync2).expect("fs2");
    let ino2 = fs2.find_inode("app.log").expect("visible after remount");
    assert_eq!(fs2.apply_read(ino2, 0, 100).unwrap(), b"segredo local".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn spooled_write_drains_and_commits_correct_content() {
    use git_drive::fs::GitDriveFS;

    let test = init_repo();
    commit_file(&test.repo, "seed.txt", b"x", "seed");
    let repo_path = test.dir.path().to_str().unwrap();
    let sync = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs = GitDriveFS::new_with_options(
        repo_path,
        "main".to_string(),
        sync,
        64 * 1024 * 1024,
        8, // tiny threshold: forces the spooled regime
    )
    .expect("fs");

    let (ino, _) = fs.apply_create(1, "video.bin").unwrap();
    let payload: Vec<u8> = (0..1000u32).flat_map(|i| i.to_le_bytes()).collect();
    fs.apply_write(ino, 0, &payload).unwrap();
    fs.apply_flush(ino);
    wait_flush().await;

    assert_eq!(
        read_file(test.dir.path(), "video.bin"),
        payload,
        "spooled content should arrive intact in the working tree"
    );
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(
        tree.get_path(std::path::Path::new("video.bin")).is_ok(),
        "spooled file should be committed normally"
    );
}

// ---- Phase 2.6: chunking in the drain (conversion, pre-commit sweep, GC) ----

use git_drive::chunk::{shard_rel_path, Manifest};

/// No RAW blob above the limit in the HEAD tree — manifests are exempt
/// (at test scale the manifest may exceed the tiny threshold; in production
/// it is orders of magnitude smaller than the threshold).
fn tree_has_no_blob_above(repo: &Repository, limit: u64) -> bool {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    let mut ok = true;
    tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob) {
            if let Ok(obj) = entry.to_object(repo) {
                if let Some(blob) = obj.as_blob() {
                    if blob.size() as u64 > limit && !Manifest::is_manifest_bytes(blob.content()) {
                        ok = false;
                    }
                }
            }
        }
        git2::TreeWalkResult::Ok
    })
    .unwrap();
    ok
}

fn read_manifest_at(dir: &std::path::Path, rel: &str) -> Manifest {
    let bytes = std::fs::read(dir.join(rel)).expect("manifest should exist in the working tree");
    assert!(
        Manifest::is_manifest_bytes(&bytes),
        "file should be a manifest, got: {:?}...",
        &bytes[..bytes.len().min(40)]
    );
    Manifest::parse(&bytes).expect("parseable manifest")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunked_write_commits_manifest_and_chunks() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_chunking(64, 32); // 64-byte threshold, 32-byte chunks (test)

    let payload: Vec<u8> = (0..200u8).collect();
    sync.queue_write("grande.bin", payload.clone());
    wait_flush().await;

    // Working tree: manifest at the path, intact chunks in .chunks/
    let manifest = read_manifest_at(test.dir.path(), "grande.bin");
    assert_eq!(manifest.total_size, 200);
    assert_eq!(manifest.hashes.len(), 7); // ceil(200/32)
    let mut reassembled = Vec::new();
    for h in &manifest.hashes {
        reassembled.extend_from_slice(&std::fs::read(test.dir.path().join(shard_rel_path(h))).unwrap());
    }
    assert_eq!(reassembled, payload, "reassembled chunks should reproduce the original");

    // Commit: manifest + chunks in the tree, no blob above the threshold
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(tree.get_path(std::path::Path::new("grande.bin")).is_ok());
    assert!(
        tree.get_path(&shard_rel_path(&manifest.hashes[0])).is_ok(),
        "chunks should be committed"
    );
    assert!(
        tree_has_no_blob_above(&test.repo, 64),
        "no blob above the threshold may be staged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn precommit_sweep_converts_raw_file_written_outside_fuse() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_chunking(64, 32);

    // A large file reaches the working tree from OUTSIDE the queue (the zip
    // from the incident): the pre-commit sweep must convert it anyway.
    let payload = vec![9u8; 150];
    std::fs::write(test.dir.path().join("externo.bin"), &payload).unwrap();
    sync.queue_write("gatilho.txt", b"pequeno".to_vec());
    wait_flush().await;

    let manifest = read_manifest_at(test.dir.path(), "externo.bin");
    assert_eq!(manifest.total_size, 150);
    assert!(tree_has_no_blob_above(&test.repo, 64));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunked_update_and_delete_gc_orphan_chunks() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_chunking(64, 32);

    sync.queue_write("dado.bin", vec![1u8; 100]);
    wait_flush().await;
    let old = read_manifest_at(test.dir.path(), "dado.bin");

    // Update: new content -> new manifest, orphaned old chunks disappear
    sync.queue_write("dado.bin", vec![2u8; 100]);
    wait_flush().await;
    let new = read_manifest_at(test.dir.path(), "dado.bin");
    assert_ne!(old.hashes, new.hashes);
    for h in &old.hashes {
        assert!(
            !test.dir.path().join(shard_rel_path(h)).exists(),
            "orphaned chunk from the old manifest should be removed from the working tree"
        );
    }
    for h in &new.hashes {
        assert!(test.dir.path().join(shard_rel_path(h)).exists());
    }

    // Delete: manifest and chunks disappear
    sync.queue_delete("dado.bin");
    wait_flush().await;
    assert!(!test.dir.path().join("dado.bin").exists());
    for h in &new.hashes {
        assert!(!test.dir.path().join(shard_rel_path(h)).exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn shared_chunk_survives_delete_of_one_referencing_file() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_chunking(64, 32);

    let payload = vec![5u8; 100];
    sync.queue_write("a.bin", payload.clone());
    sync.queue_write("b.bin", payload.clone());
    wait_flush().await;

    let ma = read_manifest_at(test.dir.path(), "a.bin");
    let mb = read_manifest_at(test.dir.path(), "b.bin");
    assert_eq!(ma.hashes, mb.hashes, "identical content shares chunks (dedup)");

    sync.queue_delete("a.bin");
    wait_flush().await;
    for h in &mb.hashes {
        assert!(
            test.dir.path().join(shard_rel_path(h)).exists(),
            "a chunk still referenced by b.bin must not be collected"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunking_skips_ignored_local_only_files() {
    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"*.log\n", "gitignore");
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_chunking(64, 32);

    // A large local-only file stays INTACT (not committed; chunking it would break reads)
    std::fs::write(test.dir.path().join("grande.log"), vec![3u8; 200]).unwrap();
    sync.queue_write("gatilho.txt", b"x".to_vec());
    wait_flush().await;

    let bytes = std::fs::read(test.dir.path().join("grande.log")).unwrap();
    assert_eq!(bytes, vec![3u8; 200], "an ignored file must not be converted into a manifest");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sweep_conversion_raises_refresh_so_mount_sees_new_chunked_file() {
    use git_drive::fs::GitDriveFS;

    let test = init_repo();
    commit_file(&test.repo, "seed.txt", b"x", "seed");
    let repo_path = test.dir.path().to_str().unwrap();
    let sync = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    sync.set_chunking(64, 32);

    // Mount active BEFORE the large file arrives from outside FUSE
    let mut fs = GitDriveFS::new(repo_path, "main".to_string(), sync.clone()).expect("fs");
    assert_eq!(fs.find_inode("externo.bin"), None);

    let payload = vec![8u8; 150];
    std::fs::write(test.dir.path().join("externo.bin"), &payload).unwrap();
    sync.queue_write("gatilho.txt", b"x".to_vec());
    wait_flush().await;

    // The sweep's conversion created a new path in HEAD that the mount's
    // index does not know about — the drain must raise the refresh flag.
    fs.refresh_if_needed();
    let ino = fs
        .find_inode("externo.bin")
        .expect("converted file should appear in the mount after the refresh");
    assert_eq!(fs.apply_getattr(ino).unwrap().size, 150, "real size, not the manifest's");
    assert_eq!(fs.apply_read(ino, 0, 200).unwrap(), payload);
}

// ---- Phase 2.7: encryption at rest in the drain ----

use git_drive::crypt::Crypto;
use std::sync::Arc;

fn test_crypto() -> Arc<Crypto> {
    Arc::new(Crypto::from_master_key(&[42u8; 32]))
}

/// No entry name nor blob byte in the HEAD tree contains the given plaintext
/// terms ("opaque repo" invariant).
fn tree_leaks_plaintext(repo: &Repository, terms: &[&str]) -> bool {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    let mut leaked = false;
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        let name = entry.name().unwrap_or("");
        let full = format!("{}{}", root, name);
        for t in terms {
            if full.contains(t) {
                leaked = true;
            }
        }
        if entry.kind() == Some(git2::ObjectType::Blob) {
            if let Ok(obj) = entry.to_object(repo) {
                if let Some(blob) = obj.as_blob() {
                    let content = blob.content();
                    for t in terms {
                        if content
                            .windows(t.len())
                            .any(|w| w == t.as_bytes())
                        {
                            leaked = true;
                        }
                    }
                }
            }
        }
        git2::TreeWalkResult::Ok
    })
    .unwrap();
    leaked
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_drain_commits_no_plaintext_names_or_content() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let crypto = test_crypto();
    sync.set_crypto(Some(crypto.clone()));

    sync.queue_mkdir("Pasta Pessoal");
    sync.queue_write("Pasta Pessoal/segredo.txt", b"conteudo confidencial".to_vec());
    sync.queue_symlink("atalho.txt", "Pasta Pessoal/segredo.txt");
    wait_flush().await;

    assert!(
        !tree_leaks_plaintext(&test.repo, &["segredo", "Pasta", "Pessoal", "atalho", "confidencial"]),
        "the HEAD tree must not contain plaintext names or content"
    );

    // Content recoverable with the key: find the encrypted file in the worktree
    let cipher_rel = crypto.enc_path("Pasta Pessoal/segredo.txt");
    let sealed = std::fs::read(test.dir.path().join(&cipher_rel)).expect("encrypted file in the worktree");
    assert!(Crypto::is_encrypted_bytes(&sealed));
    assert_eq!(crypto.open(&sealed).unwrap(), b"conteudo confidencial".to_vec());

    // Real symlink with a per-component encrypted target (mode 120000 preserved)
    let link_rel = crypto.enc_path("atalho.txt");
    let meta = std::fs::symlink_metadata(test.dir.path().join(&link_rel)).unwrap();
    assert!(meta.file_type().is_symlink());
    let target = std::fs::read_link(test.dir.path().join(&link_rel)).unwrap();
    // The on-disk target is the canonical encrypted path of the clear target
    // (hashed components resolve through their sidecars on the FUSE side).
    assert_eq!(
        target.to_string_lossy(),
        crypto.enc_path("Pasta Pessoal/segredo.txt")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_write_is_deterministic_no_commit_churn() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_crypto(Some(test_crypto()));

    sync.queue_write("doc.txt", b"versao unica".to_vec());
    wait_flush().await;
    let head1 = test.repo.head().unwrap().peel_to_commit().unwrap().id();

    // Same content again: identical ciphertext -> same tree -> no commit
    sync.queue_write("doc.txt", b"versao unica".to_vec());
    wait_flush().await;
    let head2 = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(head1, head2, "convergence: an identical rewrite creates no new commit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_rename_and_delete_translate_paths() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let crypto = test_crypto();
    sync.set_crypto(Some(crypto.clone()));

    sync.queue_write("original.txt", b"dados".to_vec());
    wait_flush().await;
    let old_rel = crypto.enc_path("original.txt");
    assert!(test.dir.path().join(&old_rel).exists());

    sync.queue_rename("original.txt", "renomeado.txt");
    wait_flush().await;
    let new_rel = crypto.enc_path("renomeado.txt");
    assert!(!test.dir.path().join(&old_rel).exists(), "encrypted source should be gone");
    let sealed = std::fs::read(test.dir.path().join(&new_rel)).unwrap();
    assert_eq!(crypto.open(&sealed).unwrap(), b"dados".to_vec());

    sync.queue_delete("renomeado.txt");
    wait_flush().await;
    assert!(!test.dir.path().join(&new_rel).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_chunking_seals_chunks_and_manifest() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let crypto = test_crypto();
    sync.set_crypto(Some(crypto.clone()));
    sync.set_chunking(64, 32);

    let payload: Vec<u8> = (0..200u8).collect();
    sync.queue_write("grande.bin", payload.clone());
    wait_flush().await;

    assert!(!tree_leaks_plaintext(&test.repo, &["grande"]));

    // Sealed manifest at the encrypted path; chunks are sealed files named
    // by the sha256 of the CIPHERTEXT; decrypted reassembly reproduces the original.
    let cipher_rel = crypto.enc_path("grande.bin");
    let sealed_manifest = std::fs::read(test.dir.path().join(&cipher_rel)).unwrap();
    assert!(Crypto::is_encrypted_bytes(&sealed_manifest));
    let manifest = Manifest::parse(&crypto.open(&sealed_manifest).unwrap()).unwrap();
    assert_eq!(manifest.total_size, 200, "manifest stores PLAINTEXT sizes");

    let mut reassembled = Vec::new();
    for h in &manifest.hashes {
        let sealed_chunk = std::fs::read(test.dir.path().join(shard_rel_path(h))).unwrap();
        assert_eq!(
            &git_drive::chunk::sha256_hex(&sealed_chunk),
            h,
            "chunk hash is over the ciphertext (GC/integrity without the key)"
        );
        reassembled.extend_from_slice(&crypto.open(&sealed_chunk).unwrap());
    }
    assert_eq!(reassembled, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn enable_migration_encrypts_existing_worktree() {
    let test = init_repo();
    commit_file(&test.repo, "antigo.txt", b"historico claro", "add antigo");
    commit_file(&test.repo, "docs/nota.md", b"outra nota", "add nota");

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let crypto = test_crypto();
    sync.set_crypto(Some(crypto.clone()));

    // Any activity triggers the drain; the sweep migrates the rest
    sync.queue_write("gatilho.txt", b"x".to_vec());
    wait_flush().await;

    assert!(
        !tree_leaks_plaintext(&test.repo, &["antigo", "historico", "nota", "outra", "gatilho"]),
        "pre-existing worktree should be migrated to encrypted form"
    );
    let sealed = std::fs::read(
        test.dir.path().join(crypto.enc_path("antigo.txt")),
    )
    .expect("migrated file at the encrypted path");
    assert_eq!(crypto.open(&sealed).unwrap(), b"historico claro".to_vec());
    assert!(
        !test.dir.path().join("antigo.txt").exists(),
        "plaintext file must not remain in the worktree"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn disable_migration_decrypts_everything_back() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let crypto = test_crypto();
    sync.set_crypto(Some(crypto.clone()));
    sync.queue_write("privado/arquivo.txt", b"segredo".to_vec());
    wait_flush().await;
    assert!(!tree_leaks_plaintext(&test.repo, &["privado", "arquivo", "segredo"]));

    // Turn it off: plaintext target, key available only for the migration
    sync.set_crypto(None);
    sync.set_migration_crypto(Some(crypto.clone()));
    sync.queue_write("gatilho.txt", b"y".to_vec());
    wait_flush().await;

    let clear = std::fs::read(test.dir.path().join("privado/arquivo.txt"))
        .expect("file decrypted back at the plaintext path");
    assert_eq!(clear, b"segredo".to_vec());
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    assert!(
        head.tree().unwrap().get_path(std::path::Path::new("privado/arquivo.txt")).is_ok(),
        "tree has the plaintext path again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn enable_migration_rechunks_clear_chunked_files_sealing_chunks() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_chunking(64, 32);

    // Pre-encryption state: a chunked file committed IN PLAINTEXT
    let payload: Vec<u8> = (0..200u8).collect();
    sync.queue_write("video.bin", payload.clone());
    wait_flush().await;
    let clear_manifest = read_manifest_at(test.dir.path(), "video.bin");
    assert!(
        std::fs::read(test.dir.path().join(shard_rel_path(&clear_manifest.hashes[0])))
            .map(|b| !Crypto::is_encrypted_bytes(&b))
            .unwrap_or(false),
        "precondition: plaintext chunks"
    );

    // Enable encryption: the migration must NOT leave any plaintext chunk
    let crypto = test_crypto();
    sync.set_crypto(Some(crypto.clone()));
    sync.queue_write("gatilho.txt", b"x".to_vec());
    wait_flush().await;

    // No plaintext chunk remained in the worktree
    for entry in walkdir_files(test.dir.path().join(".chunks")) {
        let bytes = std::fs::read(&entry).unwrap();
        assert!(
            Crypto::is_encrypted_bytes(&bytes),
            "plaintext chunk survived the migration: {:?}",
            entry
        );
    }
    // Sealed manifest at the encrypted path, content intact when reassembled
    let cipher_rel = crypto.enc_path("video.bin");
    let sealed_manifest = std::fs::read(test.dir.path().join(&cipher_rel)).unwrap();
    let manifest = Manifest::parse(&crypto.open(&sealed_manifest).unwrap()).unwrap();
    assert_eq!(manifest.total_size, 200);
    let mut reassembled = Vec::new();
    for h in &manifest.hashes {
        let sealed_chunk = std::fs::read(test.dir.path().join(shard_rel_path(h))).unwrap();
        reassembled.extend_from_slice(&crypto.open(&sealed_chunk).unwrap());
    }
    assert_eq!(reassembled, payload);
    // And the plaintext path is gone
    assert!(!test.dir.path().join("video.bin").exists());
    assert!(!tree_leaks_plaintext(&test.repo, &["video"]));
}

fn walkdir_files(root: std::path::PathBuf) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn enable_migration_encrypts_preexisting_symlinks() {
    let test = init_repo();
    commit_file(&test.repo, "real.txt", b"alvo", "add real");
    // PLAINTEXT symlink committed before encryption was enabled
    let workdir = test.repo.workdir().unwrap();
    std::os::unix::fs::symlink("real.txt", workdir.join("atalho.txt")).unwrap();
    let mut index = test.repo.index().unwrap();
    index.add_path(std::path::Path::new("atalho.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = test.repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
    let parent = test.repo.head().unwrap().peel_to_commit().unwrap();
    test.repo
        .commit(Some("HEAD"), &sig, &sig, "add symlink", &tree, &[&parent])
        .unwrap();

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let crypto = test_crypto();
    sync.set_crypto(Some(crypto.clone()));
    sync.queue_write("gatilho.txt", b"x".to_vec());
    wait_flush().await;

    assert!(
        !tree_leaks_plaintext(&test.repo, &["atalho", "real"]),
        "pre-existing symlink should be migrated (name AND target encrypted)"
    );
    let cipher_link = crypto.enc_path("atalho.txt");
    let meta = std::fs::symlink_metadata(test.dir.path().join(&cipher_link))
        .expect("migrated symlink at the encrypted path");
    assert!(meta.file_type().is_symlink(), "still a symlink (mode 120000)");
    let target = std::fs::read_link(test.dir.path().join(&cipher_link)).unwrap();
    assert_eq!(crypto.dec_path(&target.to_string_lossy()), "real.txt");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pull_watcher_publishes_commits_left_unpushed() {
    // A commit that never reached the remote (push failed while offline, or
    // the previous session died before publishing) must be published by the
    // pull cycle itself — not only by the next local write.
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    commit_file(&test.repo, "base.txt", b"base", "base");
    git_drive::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();
    commit_file(&test.repo, "offline.txt", b"written offline", "offline write");
    let local_tip = test.repo.head().unwrap().peel_to_commit().unwrap().id();

    let _sync = SyncManager::new_full_with_pull(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        1,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
        Some("origin".to_string()),
        None,
        1,
    );
    tokio::time::sleep(Duration::from_millis(3500)).await;

    let bare_repo = Repository::open(bare.path()).unwrap();
    let remote_tip = bare_repo
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id();
    assert_eq!(remote_tip, local_tip, "the pull cycle published the pending commit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn journal_replays_ops_of_a_crashed_session_at_boot() {
    // Session 1 acknowledges writes and "crashes" before the debounce drain
    // (huge debounce = the drain never runs). Session 2 must replay and
    // commit them before mounting.
    let test = init_repo();
    let repo_path = test.dir.path().to_str().unwrap().to_string();
    let journal_dir = test.dir.path().join(".git/git-drive-journal");

    let s1 = SyncManager::new_with_identity(
        &repo_path,
        "main".to_string(),
        3600,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
    );
    assert_eq!(s1.enable_journal(&journal_dir).unwrap(), 0);
    s1.queue_write("notes/a.txt", b"saved right before the crash".to_vec());
    s1.queue_write("b.txt", b"to be renamed".to_vec());
    s1.queue_rename("b.txt", "c.txt");
    assert!(!file_exists(test.dir.path(), "notes/a.txt"), "nothing applied yet");

    let s2 = SyncManager::new_with_identity(
        &repo_path,
        "main".to_string(),
        3600,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
    );
    assert_eq!(s2.enable_journal(&journal_dir).unwrap(), 3);
    s2.recover_at_boot();

    assert_eq!(read_file(test.dir.path(), "notes/a.txt"), b"saved right before the crash");
    assert_eq!(read_file(test.dir.path(), "c.txt"), b"to be renamed");
    assert!(!file_exists(test.dir.path(), "b.txt"), "causal order preserved by the replay");
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(tree.get_path(std::path::Path::new("notes/a.txt")).is_ok(), "committed");
    let left = std::fs::read_dir(&journal_dir).unwrap().count();
    assert_eq!(left, 0, "journal truncated after the commit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn journal_is_truncated_by_the_regular_drain() {
    let test = init_repo();
    let journal_dir = test.dir.path().join(".git/git-drive-journal");
    let sync = mgr(test.dir.path().to_str().unwrap(), "main", Some(("Alice", "a@x.com")));
    sync.enable_journal(&journal_dir).unwrap();
    sync.queue_write("a.txt", b"x".to_vec());
    assert!(std::fs::read_dir(&journal_dir).unwrap().count() > 0, "journaled on enqueue");
    wait_flush().await;
    assert_eq!(read_file(test.dir.path(), "a.txt"), b"x");
    assert_eq!(std::fs::read_dir(&journal_dir).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn boot_recovery_commits_a_dirty_working_tree() {
    let test = init_repo();
    std::fs::write(test.dir.path().join("left-dirty.txt"), b"applied, never committed").unwrap();
    let sync = mgr(test.dir.path().to_str().unwrap(), "main", Some(("Alice", "a@x.com")));
    sync.recover_at_boot();
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(head.message().unwrap(), "git-drive: recover uncommitted changes");
    assert!(head.tree().unwrap().get_path(std::path::Path::new("left-dirty.txt")).is_ok());
}
