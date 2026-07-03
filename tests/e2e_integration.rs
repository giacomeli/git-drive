mod common;

use common::{commit_file, file_exists, init_bare_remote, init_repo};
use drive_sync::fs::GitDriveFS;
use drive_sync::sync::SyncManager;
use git2::Repository;
use std::time::Duration;

fn new_sync_with_remote(
    repo_path: &str,
    pull_interval_secs: u64,
    user: (&str, &str),
) -> SyncManager {
    SyncManager::new_full_with_pull(
        repo_path,
        "main".to_string(),
        1,
        Some(user.0.to_string()),
        Some(user.1.to_string()),
        Some("origin".to_string()),
        None,
        pull_interval_secs,
    )
}

fn new_sync_no_remote(repo_path: &str) -> SyncManager {
    SyncManager::new_with_identity(
        repo_path,
        "main".to_string(),
        1,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
    )
}

async fn wait_for_batch() {
    tokio::time::sleep(Duration::from_millis(2500)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn e2e_create_write_flush_commits_and_pushes() {
    let test = init_repo();
    let bare = init_bare_remote();
    test.repo
        .remote(
            "origin",
            &format!("file://{}", bare.path().to_str().unwrap()),
        )
        .unwrap();
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    let sync = new_sync_with_remote(
        test.dir.path().to_str().unwrap(),
        0,
        ("Alice", "a@x.com"),
    );
    let mut fs = GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();

    let (_ino, _attr) = fs.apply_create(1, "hello.txt").unwrap();
    // simular escrita: o apply_create já empilha Write vazio, aqui sobrescrevemos
    fs.sync_queue_write_for_test("hello.txt", b"world".to_vec());
    wait_for_batch().await;

    let bare_repo = Repository::open(bare.path()).unwrap();
    let bare_head = bare_repo
        .find_reference("refs/heads/main")
        .unwrap()
        .peel_to_commit()
        .unwrap();
    let tree = bare_head.tree().unwrap();
    let entry = tree.get_path(std::path::Path::new("hello.txt")).unwrap();
    let blob = entry
        .to_object(&bare_repo)
        .unwrap()
        .into_blob()
        .unwrap();
    assert_eq!(blob.content(), b"world");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn e2e_mkdir_rmdir_roundtrip() {
    let test = init_repo();
    let bare = init_bare_remote();
    test.repo
        .remote(
            "origin",
            &format!("file://{}", bare.path().to_str().unwrap()),
        )
        .unwrap();
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    let sync = new_sync_with_remote(
        test.dir.path().to_str().unwrap(),
        0,
        ("Alice", "a@x.com"),
    );
    let mut fs = GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();

    fs.apply_mkdir(1, "foo").unwrap();
    wait_for_batch().await;
    assert!(file_exists(test.dir.path(), "foo/.gitkeep"));

    fs.apply_rmdir(1, "foo").unwrap();
    wait_for_batch().await;
    assert!(!file_exists(test.dir.path(), "foo"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn e2e_rename_produces_correct_tree() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"content", "add");

    let sync = new_sync_no_remote(test.dir.path().to_str().unwrap());
    let mut fs = GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();

    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    wait_for_batch().await;

    assert!(!file_exists(test.dir.path(), "a.txt"));
    assert!(file_exists(test.dir.path(), "b.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn e2e_pull_brings_external_changes() {
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    let sync = new_sync_with_remote(
        test.dir.path().to_str().unwrap(),
        1,
        ("Alice", "a@x.com"),
    );
    let mut fs = GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();
    assert!(fs.find_inode("externo.txt").is_none());

    // clone B empurra novo commit
    let dir_b = tempfile::tempdir().unwrap();
    let repo_b = Repository::clone(&bare_url, dir_b.path()).unwrap();
    {
        let mut cfg = repo_b.config().unwrap();
        cfg.set_str("user.name", "Bob").unwrap();
        cfg.set_str("user.email", "b@x.com").unwrap();
    }
    std::fs::write(dir_b.path().join("externo.txt"), b"x").unwrap();
    let mut index = repo_b.index().unwrap();
    index.add_path(std::path::Path::new("externo.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo_b.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Bob", "b@x.com").unwrap();
    let parent = repo_b.head().unwrap().peel_to_commit().unwrap();
    repo_b
        .commit(Some("HEAD"), &sig, &sig, "add externo", &tree, &[&parent])
        .unwrap();
    drive_sync::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    // espera pelo menos 2 ciclos de pull
    tokio::time::sleep(Duration::from_millis(3500)).await;

    fs.refresh_if_needed();
    assert!(fs.find_inode("externo.txt").is_some());
}
