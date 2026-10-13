mod common;

use common::{commit_file, init_repo};
use git_drive::fs::GitDriveFS;
use git_drive::sync::SyncManager;
use fuser::FileType;

fn new_fs(repo_path: &str, branch: &str) -> GitDriveFS {
    let sync = SyncManager::new(repo_path, 5);
    GitDriveFS::new(repo_path, branch.to_string(), sync).expect("GitDriveFS::new")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async
fn new_builds_root_only_on_empty_repo() {
    let test = init_repo();
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.file_count(), 0);
    assert_eq!(fs.dir_count(), 0);
    assert_eq!(fs.find_inode(""), Some(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async
fn new_builds_nested_dir_tree() {
    let test = init_repo();
    commit_file(&test.repo, "foo/bar/baz.txt", b"x", "add baz");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.file_count(), 1);
    assert_eq!(fs.dir_count(), 2);
    assert!(fs.find_inode("foo").is_some());
    assert!(fs.find_inode("foo/bar").is_some());
    assert!(fs.find_inode("foo/bar/baz.txt").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async
fn alloc_inode_is_monotonic() {
    let test = init_repo();
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let a = fs.alloc_inode_for_test();
    let b = fs.alloc_inode_for_test();
    let c = fs.alloc_inode_for_test();
    assert!(b > a);
    assert!(c > b);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async
fn resolve_parent_for_nested_path() {
    let test = init_repo();
    commit_file(&test.repo, "foo/bar/baz.txt", b"x", "add baz");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let foo_bar_ino = fs.find_inode("foo/bar").unwrap();
    assert_eq!(fs.resolve_parent_for_test("foo/bar/baz.txt"), Some(foo_bar_ino));
    assert_eq!(fs.resolve_parent_for_test("foo/bar"), fs.find_inode("foo"));
    assert_eq!(fs.resolve_parent_for_test("top.txt"), Some(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_lookup_root_child_returns_dir() {
    let test = init_repo();
    commit_file(&test.repo, "foo/a.txt", b"x", "add");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let attr = fs.apply_lookup(1, "foo").expect("foo must exist");
    assert_eq!(attr.kind, FileType::Directory);
    assert_eq!(attr.perm, 0o755);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_lookup_nonexistent_is_enoent() {
    let test = init_repo();
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_lookup(1, "nope"), Err(libc::ENOENT));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_getattr_file_returns_regular() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"xy", "add");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();
    let attr = fs.apply_getattr(ino).unwrap();
    assert_eq!(attr.kind, FileType::RegularFile);
    assert_eq!(attr.perm, 0o644);
    assert_eq!(attr.size, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_getattr_dir_returns_directory() {
    let test = init_repo();
    commit_file(&test.repo, "foo/a.txt", b"x", "add");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("foo").unwrap();
    let attr = fs.apply_getattr(ino).unwrap();
    assert_eq!(attr.kind, FileType::Directory);
    assert_eq!(attr.perm, 0o755);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_getattr_uses_current_process_uid_gid() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();
    let attr = fs.apply_getattr(ino).unwrap();
    let expected_uid = unsafe { libc::getuid() };
    let expected_gid = unsafe { libc::getgid() };
    assert_eq!(attr.uid, expected_uid);
    assert_eq!(attr.gid, expected_gid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_getattr_mtime_is_stable_between_calls() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();
    let first = fs.apply_getattr(ino).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let second = fs.apply_getattr(ino).unwrap();
    assert_eq!(first.mtime, second.mtime);
    assert_eq!(first.ctime, second.ctime);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_getattr_unknown_ino_is_enoent() {
    let test = init_repo();
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_getattr(9999), Err(libc::ENOENT));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_readdir_root_lists_top_level_children_only() {
    let test = init_repo();
    commit_file(&test.repo, "top.txt", b"x", "add top");
    commit_file(&test.repo, "foo/nested.txt", b"y", "add nested");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let entries = fs.apply_readdir(1).unwrap();
    let names: Vec<&str> = entries.iter().map(|(_, _, n)| n.as_str()).collect();
    assert!(names.contains(&"."));
    assert!(names.contains(&".."));
    assert!(names.contains(&"top.txt"));
    assert!(names.contains(&"foo"));
    assert!(!names.contains(&"nested.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_readdir_on_file_ino_is_enoent() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();
    assert_eq!(fs.apply_readdir(ino), Err(libc::ENOENT));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_create_allocates_inode_and_registers() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, attr) = fs.apply_create(1, "novo.txt").unwrap();
    assert!(ino >= 2);
    assert_eq!(attr.kind, fuser::FileType::RegularFile);
    assert_eq!(fs.find_inode("novo.txt"), Some(ino));
    // must show up in the root readdir
    let names: Vec<_> = fs
        .apply_readdir(1)
        .unwrap()
        .into_iter()
        .map(|(_, _, n)| n)
        .collect();
    assert!(names.contains(&"novo.txt".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_create_in_subdir() {
    let test = init_repo();
    commit_file(&test.repo, "foo/x.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let foo_ino = fs.find_inode("foo").unwrap();
    let (ino, _attr) = fs.apply_create(foo_ino, "new.txt").unwrap();
    assert_eq!(fs.find_inode("foo/new.txt"), Some(ino));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_create_duplicate_is_eexist() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_create(1, "a.txt"), Err(libc::EEXIST));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_create_parent_not_dir_is_enotdir() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let a_ino = fs.find_inode("a.txt").unwrap();
    assert_eq!(fs.apply_create(a_ino, "child.txt"), Err(libc::ENOTDIR));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_create_queues_initial_empty_write() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    fs.apply_create(1, "novo.txt").unwrap();
    let ops = fs.sync_snapshot_for_test();
    // there must be a Write { path: "novo.txt", content: [] }
    assert!(ops.iter().any(|op| matches!(op,
        git_drive::sync::SyncOp::Write { path, content }
            if path == "novo.txt" && content.is_empty()
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_unlink_removes_file() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert!(fs.find_inode("a.txt").is_some());
    fs.apply_unlink(1, "a.txt").unwrap();
    assert_eq!(fs.find_inode("a.txt"), None);
    let names: Vec<_> = fs
        .apply_readdir(1)
        .unwrap()
        .into_iter()
        .map(|(_, _, n)| n)
        .collect();
    assert!(!names.contains(&"a.txt".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_unlink_nonexistent_is_enoent() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_unlink(1, "nope.txt"), Err(libc::ENOENT));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_unlink_on_directory_is_eisdir() {
    let test = init_repo();
    commit_file(&test.repo, "foo/a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_unlink(1, "foo"), Err(libc::EISDIR));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_unlink_queues_delete_op() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    fs.apply_unlink(1, "a.txt").unwrap();
    let ops = fs.sync_snapshot_for_test();
    assert!(ops.iter().any(|op| matches!(op,
        git_drive::sync::SyncOp::Delete { path } if path == "a.txt"
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_mkdir_allocates_and_registers() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, attr) = fs.apply_mkdir(1, "nova").unwrap();
    assert!(ino >= 2);
    assert_eq!(attr.kind, fuser::FileType::Directory);
    assert_eq!(fs.find_inode("nova"), Some(ino));
    let names: Vec<_> = fs
        .apply_readdir(1)
        .unwrap()
        .into_iter()
        .map(|(_, _, n)| n)
        .collect();
    assert!(names.contains(&"nova".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_mkdir_nested_via_two_calls() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (a_ino, _) = fs.apply_mkdir(1, "a").unwrap();
    let (b_ino, _) = fs.apply_mkdir(a_ino, "b").unwrap();
    assert_eq!(fs.find_inode("a"), Some(a_ino));
    assert_eq!(fs.find_inode("a/b"), Some(b_ino));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_mkdir_duplicate_is_eexist() {
    let test = init_repo();
    commit_file(&test.repo, "foo/x.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_mkdir(1, "foo"), Err(libc::EEXIST));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_mkdir_parent_not_dir_is_enotdir() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let a_ino = fs.find_inode("a.txt").unwrap();
    assert_eq!(fs.apply_mkdir(a_ino, "child"), Err(libc::ENOTDIR));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_mkdir_queues_mkdir_op() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    fs.apply_mkdir(1, "pasta").unwrap();
    let ops = fs.sync_snapshot_for_test();
    assert!(ops.iter().any(|op| matches!(op,
        git_drive::sync::SyncOp::Mkdir { path } if path == "pasta"
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rmdir_empty_directory() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    fs.apply_mkdir(1, "vazia").unwrap();
    fs.apply_rmdir(1, "vazia").unwrap();
    assert_eq!(fs.find_inode("vazia"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rmdir_non_empty_is_enotempty() {
    let test = init_repo();
    commit_file(&test.repo, "foo/a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_rmdir(1, "foo"), Err(libc::ENOTEMPTY));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rmdir_on_file_is_enotdir() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_rmdir(1, "a.txt"), Err(libc::ENOTDIR));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rmdir_queues_rmdir_op() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    fs.apply_mkdir(1, "pasta").unwrap();
    fs.apply_rmdir(1, "pasta").unwrap();
    let ops = fs.sync_snapshot_for_test();
    // After mkdir + rmdir on the same key, the buffer holds only Rmdir (last one wins).
    assert!(ops.iter().any(|op| matches!(op,
        git_drive::sync::SyncOp::Rmdir { path } if path == "pasta"
    )));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rename_file_in_same_dir() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let orig_ino = fs.find_inode("a.txt").unwrap();
    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    assert_eq!(fs.find_inode("a.txt"), None);
    assert_eq!(fs.find_inode("b.txt"), Some(orig_ino));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rename_file_across_dirs() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add a");
    commit_file(&test.repo, "foo/keep.txt", b"y", "add foo");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let foo_ino = fs.find_inode("foo").unwrap();
    let orig_ino = fs.find_inode("a.txt").unwrap();
    fs.apply_rename(1, "a.txt", foo_ino, "a.txt").unwrap();
    assert_eq!(fs.find_inode("a.txt"), None);
    assert_eq!(fs.find_inode("foo/a.txt"), Some(orig_ino));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rename_directory_updates_all_descendants() {
    let test = init_repo();
    commit_file(&test.repo, "foo/bar/baz.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let baz_ino = fs.find_inode("foo/bar/baz.txt").unwrap();
    let bar_ino = fs.find_inode("foo/bar").unwrap();
    fs.apply_rename(1, "foo", 1, "qux").unwrap();
    assert_eq!(fs.find_inode("foo"), None);
    assert_eq!(fs.find_inode("foo/bar/baz.txt"), None);
    assert_eq!(fs.find_inode("qux/bar"), Some(bar_ino));
    assert_eq!(fs.find_inode("qux/bar/baz.txt"), Some(baz_ino));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rename_to_existing_file_overwrites() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add a");
    commit_file(&test.repo, "b.txt", b"y", "add b");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let orig_a = fs.find_inode("a.txt").unwrap();
    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    assert_eq!(fs.find_inode("a.txt"), None);
    assert_eq!(fs.find_inode("b.txt"), Some(orig_a));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rename_to_existing_dir_is_eexist() {
    let test = init_repo();
    commit_file(&test.repo, "foo/x.txt", b"x", "add");
    commit_file(&test.repo, "bar/y.txt", b"y", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_rename(1, "foo", 1, "bar"), Err(libc::EEXIST));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rename_nonexistent_is_enoent() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_rename(1, "nope", 1, "other"), Err(libc::ENOENT));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_rename_of_file_queues_write_plus_delete() {
    // A file rename is translated into Write(dest, content) + Delete(src) to
    // decouple it from disk dependencies (the src may never have been materialized
    // if earlier operations were coalesced in the buffer).
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    let ops = fs.sync_snapshot_for_test();
    assert!(
        ops.iter().any(|op| matches!(op,
            git_drive::sync::SyncOp::Write { path, content }
                if path == "b.txt" && content.to_vec() == b"hello"
        )),
        "expected Write(b.txt) with the original content, got {:?}",
        ops
    );
    assert!(
        ops.iter().any(|op| matches!(op,
            git_drive::sync::SyncOp::Delete { path } if path == "a.txt"
        )),
        "expected Delete(a.txt), got {:?}",
        ops
    );
}


#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn refresh_preserves_next_inode_when_paths_unchanged() {
    // Regression RF7 (Phase 0): `refresh_if_needed` used
    // `previous.get(...).unwrap_or(&self.alloc_inode())`, which evaluates
    // `alloc_inode()` ALWAYS (eager argument) — the inode counter jumped by
    // the total number of paths on every refresh, even with no change at all.
    let test = init_repo();
    commit_file(&test.repo, "foo/bar/baz.txt", b"x", "add baz");
    commit_file(&test.repo, "a.txt", b"y", "add a");

    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new(test.dir.path().to_str().unwrap(), "main".to_string(), sync)
        .expect("GitDriveFS::new");

    let ino_foo = fs.find_inode("foo").unwrap();
    let ino_baz = fs.find_inode("foo/bar/baz.txt").unwrap();
    let before = fs.alloc_inode_for_test();

    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();

    let after = fs.alloc_inode_for_test();
    assert_eq!(
        after,
        before + 1,
        "refresh with unchanged paths must not allocate new inodes"
    );
    // Existing inodes preserved
    assert_eq!(fs.find_inode("foo"), Some(ino_foo));
    assert_eq!(fs.find_inode("foo/bar/baz.txt"), Some(ino_baz));
}

// ---- Regression RF1 (Phase 0): write buffer initialized with current content ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_write_append_preserves_existing_content() {
    // Before the fix, the buffer was born empty and zero-filled up to the offset:
    // an append (`echo x >> file`) replaced the original content with zeros.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    let written = fs.apply_write(ino, 5, b" world").unwrap();
    assert_eq!(written, 6);
    assert_eq!(
        fs.read_content_for_test(ino).as_deref(),
        Some(b"hello world".as_slice())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_write_partial_overwrite_preserves_tail() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello world", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_write(ino, 0, b"J").unwrap();
    assert_eq!(
        fs.read_content_for_test(ino).as_deref(),
        Some(b"Jello world".as_slice())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_write_updates_entry_size() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hi", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_write(ino, 2, b" there").unwrap();
    let attr = fs.apply_getattr(ino).unwrap();
    assert_eq!(attr.size, 8);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_truncate_smaller_preserves_prefix() {
    // Before the fix, truncating a file with no buffer created an empty buffer
    // plus resize(N, 0) — the content became N bytes of zeros.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello world", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_truncate(ino, 4).unwrap();
    assert_eq!(
        fs.read_content_for_test(ino).as_deref(),
        Some(b"hell".as_slice())
    );
    // Truncate materializes the op immediately (there may be no flush afterwards)
    let ops = fs.sync_snapshot_for_test();
    assert!(
        ops.iter().any(|op| matches!(op,
            git_drive::sync::SyncOp::Write { path, content }
                if path == "a.txt" && content.to_vec() == b"hell"
        )),
        "expected truncated Write(a.txt), got {:?}",
        ops
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_truncate_larger_zero_fills_suffix_only() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hi", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_truncate(ino, 4).unwrap();
    assert_eq!(
        fs.read_content_for_test(ino).as_deref(),
        Some(b"hi\0\0".as_slice())
    );
}

// ---- Regression RF2 (Phase 0): flush only enqueues real writes (dirty flag) ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn flush_without_write_does_not_enqueue() {
    // flush runs on every close(2), including of an fd opened read-only.
    // Before the fix, any existing buffer was re-enqueued — potentially
    // reverting merged remote changes with stale content.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    assert!(fs.apply_flush(ino).is_none());
    assert!(fs.sync_snapshot_for_test().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn flush_after_write_enqueues_once() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_write(ino, 5, b" world").unwrap();
    assert_eq!(fs.apply_flush(ino).as_deref(), Some("a.txt"));
    let ops = fs.sync_snapshot_for_test();
    assert!(
        ops.iter().any(|op| matches!(op,
            git_drive::sync::SyncOp::Write { path, content }
                if path == "a.txt" && content.to_vec() == b"hello world"
        )),
        "expected Write(a.txt) with the full content, got {:?}",
        ops
    );

    // A second flush with no new write (e.g. a read close) does not re-enqueue
    assert!(fs.apply_flush(ino).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn truncate_clears_dirty_mark() {
    // apply_truncate already materializes the op — the following flush must not duplicate it.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_write(ino, 0, b"H").unwrap();
    fs.apply_truncate(ino, 4).unwrap();
    assert!(fs.apply_flush(ino).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn rename_after_write_clears_dirty_mark() {
    // A file rename enqueues the current content (Write(dest)+Delete(src));
    // a later flush of the same inode must not enqueue again.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_write(ino, 5, b"!").unwrap();
    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    assert!(fs.apply_flush(ino).is_none());
}

// ---- Phase 1 RF1: blob cache and reads without HEAD misses ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_read_populates_cache_and_hits_on_second_read() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"hello");
    assert!(fs.read_cache_contains_for_test(ino));
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"hello");
    let (hits, misses) = fs.read_cache_stats_for_test();
    assert_eq!((hits, misses), (1, 1), "second read must be a hit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_read_respects_offset_and_size() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello world", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    assert_eq!(fs.apply_read(ino, 6, 5).unwrap(), b"world");
    assert_eq!(fs.apply_read(ino, 100, 5).unwrap(), b"");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn read_cache_cleared_on_refresh() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new(test.dir.path().to_str().unwrap(), "main".to_string(), sync)
        .expect("GitDriveFS::new");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_read(ino, 0, 100).unwrap();
    assert_eq!(fs.read_cache_len_for_test(), 1);

    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();
    assert_eq!(fs.read_cache_len_for_test(), 0, "refresh must clear the cache");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn read_cache_eviction_respects_budget() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"aaaa", "add a");
    commit_file(&test.repo, "b.txt", b"bbbb", "add b");
    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let mut fs = GitDriveFS::new_with_cache_budget(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
        6, // only one 4-byte blob fits at a time
    )
    .expect("GitDriveFS::new_with_cache_budget");
    let ino_a = fs.find_inode("a.txt").unwrap();
    let ino_b = fs.find_inode("b.txt").unwrap();

    fs.apply_read(ino_a, 0, 100).unwrap();
    fs.apply_read(ino_b, 0, 100).unwrap();
    assert_eq!(fs.read_cache_len_for_test(), 1, "budget forces eviction");
    assert!(fs.read_cache_contains_for_test(ino_b));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_read_after_create_returns_empty_not_eio() {
    // Regression for the gap found in Phase 1: the freshly created path does not
    // exist in HEAD — without the empty buffer, read returned EIO until the first commit.
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, _) = fs.apply_create(1, "novo.txt").unwrap();

    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_read_after_rename_returns_content_not_eio() {
    // Same gap for rename: the NEW path does not exist in HEAD until the commit.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"hello");
}

// ---- Phase 1 RF2: implicit swap validation ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn implicit_swap_with_two_files_in_source_returns_exdev() {
    // The safe-save contract is a temp dir with ONE file. With two, picking
    // either is a guess — the code used to take the first one in HashSet
    // iteration order and could swap the wrong file's content.
    let test = init_repo();
    commit_file(&test.repo, "tmpdir/a.txt", b"aaa", "add a");
    commit_file(&test.repo, "tmpdir/b.txt", b"bbb", "add b");
    commit_file(&test.repo, "a.txt", b"root-a", "add root a");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let tmpdir_ino = fs.find_inode("tmpdir").unwrap();
    let root_a = fs.find_inode("a.txt").unwrap();

    // RENAME with empty names = the exchangedata encoding in macFUSE
    let result = fs.apply_rename(tmpdir_ino, "", 1, "");
    assert_eq!(result, Err(libc::EXDEV));

    // No content changed, no op enqueued
    assert_eq!(fs.read_content_for_test(root_a).as_deref(), Some(b"root-a".as_slice()));
    assert!(fs.sync_snapshot_for_test().is_empty());
}

// ---- Phase 1 RF3: renames involving ephemeral paths ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn dir_rename_to_ephemeral_queues_recursive_removal() {
    // Previously, the filter dropped the entire Rename: disk kept the dir while
    // the in-memory index said it had become ephemeral — divergence.
    let test = init_repo();
    commit_file(&test.repo, "d/f.txt", b"conteudo-f", "add f");
    commit_file(&test.repo, "d/sub/g.txt", b"conteudo-g", "add g");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let f_ino = fs.find_inode("d/f.txt").unwrap();

    fs.apply_rename(1, "d", 1, "._d").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use git_drive::sync::SyncOp;
    assert!(
        !ops.iter().any(|op| matches!(op, SyncOp::Rename { .. })),
        "there must be no Rename in the queue, got {:?}",
        ops
    );
    assert!(ops.iter().any(|op| matches!(op, SyncOp::Delete { path } if path == "d/f.txt")));
    assert!(ops.iter().any(|op| matches!(op, SyncOp::Delete { path } if path == "d/sub/g.txt")));
    let pos_sub = ops.iter().position(|op| matches!(op, SyncOp::Rmdir { path } if path == "d/sub"));
    let pos_root = ops.iter().position(|op| matches!(op, SyncOp::Rmdir { path } if path == "d"));
    assert!(pos_sub.is_some() && pos_root.is_some(), "expected rmdirs, got {:?}", ops);
    assert!(pos_sub < pos_root, "rmdir must be bottom-up");

    // Content stays readable inside the ephemeral area (Trash)
    assert_eq!(fs.apply_read(f_ino, 0, 100).unwrap(), b"conteudo-f");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn dir_rename_from_ephemeral_materializes_content() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    // Ephemeral dir built in memory (all its ops are filtered out of the queue)
    let (dir_ino, _) = fs.apply_mkdir(1, "._hidden").unwrap();
    let (f_ino, _) = fs.apply_create(dir_ino, "f.txt").unwrap();
    fs.apply_write(f_ino, 0, b"segredo").unwrap();
    fs.apply_flush(f_ino);
    assert!(fs.sync_snapshot_for_test().is_empty(), "ephemeral ops must have been filtered out");

    fs.apply_rename(1, "._hidden", 1, "visible").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use git_drive::sync::SyncOp;
    let pos_mkdir = ops.iter().position(|op| matches!(op, SyncOp::Mkdir { path } if path == "visible"));
    let pos_write = ops.iter().position(|op| matches!(op,
        SyncOp::Write { path, content } if path == "visible/f.txt" && content.to_vec() == b"segredo"));
    assert!(pos_mkdir.is_some(), "expected mkdir, got {:?}", ops);
    assert!(pos_write.is_some(), "expected write with content, got {:?}", ops);
    assert!(pos_mkdir < pos_write, "mkdir must precede the write");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn file_rename_to_ephemeral_queues_only_delete() {
    // Regression test for the current (correct) behavior: Write(ephemeral dest)
    // is filtered, Delete(real src) goes through — the file leaves the repo.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    fs.apply_rename(1, "a.txt", 1, "._a.txt").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use git_drive::sync::SyncOp;
    assert_eq!(ops.len(), 1, "expected only the Delete, got {:?}", ops);
    assert!(matches!(&ops[0], SyncOp::Delete { path } if path == "a.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn file_rename_from_ephemeral_materializes_write() {
    // Regression test for the current (correct) behavior: Write(real dest) goes
    // through with the current content; Delete(ephemeral src) is filtered.
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, _) = fs.apply_create(1, "._tmp.txt").unwrap();
    fs.apply_write(ino, 0, b"data").unwrap();
    fs.apply_flush(ino);
    assert!(fs.sync_snapshot_for_test().is_empty());

    fs.apply_rename(1, "._tmp.txt", 1, "real.txt").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use git_drive::sync::SyncOp;
    assert_eq!(ops.len(), 1, "expected only the Write, got {:?}", ops);
    assert!(matches!(&ops[0], SyncOp::Write { path, content }
        if path == "real.txt" && content.to_vec() == b"data"));
}

// ---- macFUSE RENAME parse fix: exchange with real names ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_exchange_swaps_contents_by_name() {
    // With fuse_rename_in fixed in vendor, the safe-save swap arrives with
    // real names and uses apply_exchange (same logic as FUSE_EXCHANGE).
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"conteudo-a", "add a");
    commit_file(&test.repo, "b.txt", b"conteudo-b", "add b");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino_a = fs.find_inode("a.txt").unwrap();
    let ino_b = fs.find_inode("b.txt").unwrap();

    fs.apply_exchange(1, "a.txt", 1, "b.txt").unwrap();

    assert_eq!(fs.read_content_for_test(ino_a).as_deref(), Some(b"conteudo-b".as_slice()));
    assert_eq!(fs.read_content_for_test(ino_b).as_deref(), Some(b"conteudo-a".as_slice()));
    let ops = fs.sync_snapshot_for_test();
    use git_drive::sync::SyncOp;
    assert!(ops.iter().any(|op| matches!(op, SyncOp::Write { path, content }
        if path == "a.txt" && content.to_vec() == b"conteudo-b")));
    assert!(ops.iter().any(|op| matches!(op, SyncOp::Write { path, content }
        if path == "b.txt" && content.to_vec() == b"conteudo-a")));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_exchange_missing_side_is_enoent() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_exchange(1, "a.txt", 1, "nope.txt"), Err(libc::ENOENT));
}

// ---- Phase 2 RF5/RF6: native Trash and hidden .gitkeep ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn gitkeep_is_hidden_and_empty_dir_accepts_rmdir_after_refresh() {
    // Regression for the manual validation finding: after refresh, .gitkeep
    // became a visible child and rmdir of an "empty" folder failed with ENOTEMPTY.
    let test = init_repo();
    commit_file(&test.repo, "vazia/.gitkeep", b"", "add dir vazia");
    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new(test.dir.path().to_str().unwrap(), "main".to_string(), sync)
        .expect("GitDriveFS::new");

    assert_eq!(fs.file_count(), 0, ".gitkeep must not enter the indexes");
    let dir_ino = fs.find_inode("vazia").unwrap();
    let entries = fs.apply_readdir(dir_ino).unwrap();
    assert_eq!(entries.len(), 2, "only . and .. — got {:?}", entries);

    // After refresh, the rule still holds
    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();
    assert_eq!(fs.file_count(), 0);
    assert_eq!(fs.apply_rmdir(1, "vazia"), Ok(()), "rmdir of an empty dir must work");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mkdir_trashes_at_root_is_allowed_in_memory() {
    // Phase 2: native Trash — the EPERM was removed; the dir is ephemeral (it
    // does not enter the sync queue).
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, _) = fs.apply_mkdir(1, ".Trashes").unwrap();
    assert!(fs.find_inode(".Trashes").is_some());
    assert!(fs.sync_snapshot_for_test().is_empty(), "ephemeral dir op is filtered");
    let (sub, _) = fs.apply_mkdir(ino, "501").unwrap();
    assert!(sub > 0);
}

// ---- Phase 2.5, Block B: symlinks ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn symlink_in_head_appears_with_symlink_kind() {
    let test = init_repo();
    commit_file(&test.repo, "real.txt", b"data", "add real");
    common::commit_symlink(&test.repo, "alias.txt", "real.txt", "add alias");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    let ino = fs.find_inode("alias.txt").expect("alias must be in the index");
    assert_eq!(fs.apply_getattr(ino).unwrap().kind, FileType::Symlink);
    assert_eq!(fs.apply_lookup(1, "alias.txt").unwrap().kind, FileType::Symlink);

    // Regular file does not regress
    let rino = fs.find_inode("real.txt").unwrap();
    assert_eq!(fs.apply_getattr(rino).unwrap().kind, FileType::RegularFile);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_readlink_returns_target_text() {
    let test = init_repo();
    commit_file(&test.repo, "docs/real.md", b"conteudo", "add real");
    common::commit_symlink(&test.repo, "docs/alias.md", "real.md", "add alias");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    let ino = fs.find_inode("docs/alias.md").unwrap();
    assert_eq!(fs.apply_readlink(ino).unwrap(), b"real.md".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_readlink_on_regular_file_is_einval() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();
    assert_eq!(fs.apply_readlink(ino), Err(libc::EINVAL));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn readdir_reports_symlink_kind() {
    let test = init_repo();
    commit_file(&test.repo, "real.txt", b"data", "add real");
    common::commit_symlink(&test.repo, "alias.txt", "real.txt", "add alias");
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    let entries = fs.apply_readdir(1).unwrap();
    let alias = entries.iter().find(|(_, _, n)| n == "alias.txt").unwrap();
    assert_eq!(alias.1, FileType::Symlink);
    let real = entries.iter().find(|(_, _, n)| n == "real.txt").unwrap();
    assert_eq!(real.1, FileType::RegularFile);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn symlink_kind_survives_refresh() {
    let test = init_repo();
    common::commit_symlink(&test.repo, "alias.txt", "alvo.txt", "add alias");
    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new(test.dir.path().to_str().unwrap(), "main".to_string(), sync)
        .expect("GitDriveFS::new");

    let ino = fs.find_inode("alias.txt").unwrap();
    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();
    assert_eq!(fs.find_inode("alias.txt"), Some(ino), "inode preserved across refresh");
    assert_eq!(fs.apply_getattr(ino).unwrap().kind, FileType::Symlink);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_symlink_creates_entry_and_enqueues_op() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    let (ino, attr) = fs.apply_symlink(1, "alias.txt", "real.txt").expect("apply_symlink");
    assert_eq!(attr.kind, FileType::Symlink);
    assert_eq!(attr.size, "real.txt".len() as u64);
    assert_eq!(fs.find_inode("alias.txt"), Some(ino));
    assert_eq!(fs.apply_readlink(ino).unwrap(), b"real.txt".to_vec());

    let snap = fs.sync_snapshot_for_test();
    assert_eq!(snap.len(), 1);
    assert!(
        matches!(&snap[0], git_drive::sync::SyncOp::Symlink { path, target }
            if path == "alias.txt" && target == "real.txt"),
        "expected SyncOp::Symlink, got {:?}",
        snap
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_symlink_duplicate_name_is_eexist() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"x", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(
        fs.apply_symlink(1, "a.txt", "alvo").unwrap_err(),
        libc::EEXIST
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_unlink_of_symlink_queues_delete() {
    let test = init_repo();
    common::commit_symlink(&test.repo, "alias.txt", "alvo.txt", "add alias");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert_eq!(fs.apply_unlink(1, "alias.txt"), Ok(()));
    assert_eq!(fs.find_inode("alias.txt"), None);
    let snap = fs.sync_snapshot_for_test();
    assert!(
        matches!(&snap[..], [git_drive::sync::SyncOp::Delete { path }] if path == "alias.txt"),
        "expected Delete of the symlink, got {:?}",
        snap
    );
}

// ---- Phase 2.5, Block A: ignored files visible as local-only ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ignored_file_in_worktree_visible_after_boot() {
    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"*.log\n", "add gitignore");
    std::fs::write(test.dir.path().join("app.log"), b"local data").unwrap();

    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("app.log").expect("ignored file must be visible at boot");
    assert_eq!(fs.apply_getattr(ino).unwrap().size, 10);
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"local data".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ignored_file_inside_ignored_dir_creates_parent_chain() {
    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"build/\n", "add gitignore");
    std::fs::create_dir_all(test.dir.path().join("build")).unwrap();
    std::fs::write(test.dir.path().join("build/out.bin"), b"bin").unwrap();

    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let dir_ino = fs.find_inode("build").expect("the ignored file's dir must exist in the index");
    let ino = fs.find_inode("build/out.bin").expect("file inside ignored dir visible");
    let entries = fs.apply_readdir(dir_ino).unwrap();
    assert!(entries.iter().any(|(_, _, n)| n == "out.bin"));
    assert_eq!(fs.apply_read(ino, 0, 10).unwrap(), b"bin".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ignored_file_survives_refresh_with_same_inode() {
    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"*.log\n", "add gitignore");
    std::fs::write(test.dir.path().join("app.log"), b"conteudo").unwrap();

    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new(test.dir.path().to_str().unwrap(), "main".to_string(), sync)
        .expect("GitDriveFS::new");
    let ino = fs.find_inode("app.log").expect("visible at boot");

    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();
    assert_eq!(
        fs.find_inode("app.log"),
        Some(ino),
        "local-only survives refresh keeping its inode"
    );
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"conteudo".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ignored_file_delete_removes_and_queues() {
    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"*.log\n", "add gitignore");
    std::fs::write(test.dir.path().join("app.log"), b"x").unwrap();

    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    assert!(fs.find_inode("app.log").is_some());
    assert_eq!(fs.apply_unlink(1, "app.log"), Ok(()));
    assert_eq!(fs.find_inode("app.log"), None);
    let snap = fs.sync_snapshot_for_test();
    assert!(
        matches!(&snap[..], [git_drive::sync::SyncOp::Delete { path }] if path == "app.log"),
        "a local-only delete goes through the normal queue: {:?}",
        snap
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ignored_symlink_in_worktree_visible_with_kind() {
    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"*.lnk\n", "add gitignore");
    std::os::unix::fs::symlink("alvo.txt", test.dir.path().join("a.lnk")).unwrap();

    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.lnk").expect("ignored symlink visible");
    assert_eq!(fs.apply_getattr(ino).unwrap().kind, FileType::Symlink);
    assert_eq!(fs.apply_readlink(ino).unwrap(), b"alvo.txt".to_vec());
}

// ---- Phase 2.5, Block C: mtime derived from the last commit per path ----

fn epoch_time(secs: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mtime_after_boot_derives_from_last_commit_per_path() {
    let test = init_repo();
    common::commit_file_at(&test.repo, "antigo.txt", b"v1", 1_600_000_000, "add antigo");
    common::commit_file_at(&test.repo, "novo.txt", b"x", 1_700_000_000, "add novo");

    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let antigo = fs.find_inode("antigo.txt").unwrap();
    let novo = fs.find_inode("novo.txt").unwrap();
    assert_eq!(
        fs.apply_getattr(antigo).unwrap().mtime,
        epoch_time(1_600_000_000),
        "mtime must come from the last commit that touched the path, not from boot"
    );
    assert_eq!(fs.apply_getattr(novo).unwrap().mtime, epoch_time(1_700_000_000));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mtime_touched_in_session_has_precedence_over_derived() {
    let test = init_repo();
    common::commit_file_at(&test.repo, "a.txt", b"v1", 1_600_000_000, "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    let before = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    fs.apply_write(ino, 0, b"v2").unwrap();
    fs.apply_flush(ino);
    let mtime = fs.apply_getattr(ino).unwrap().mtime;
    assert!(
        mtime > before,
        "a file touched in the session uses the current clock, not the derived one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn mtime_derived_updates_after_refresh() {
    let test = init_repo();
    common::commit_file_at(&test.repo, "a.txt", b"v1", 1_600_000_000, "add");

    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new(test.dir.path().to_str().unwrap(), "main".to_string(), sync)
        .expect("GitDriveFS::new");
    let ino = fs.find_inode("a.txt").unwrap();
    assert_eq!(fs.apply_getattr(ino).unwrap().mtime, epoch_time(1_600_000_000));

    // Simulates a remote change arriving via pull: new commit + refresh flag
    common::commit_file_at(&test.repo, "a.txt", b"v2", 1_700_000_000, "update");
    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();

    assert_eq!(
        fs.apply_getattr(ino).unwrap().mtime,
        epoch_time(1_700_000_000),
        "a path touched by the new commit updates the derived mtime on refresh"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn setattr_atime_only_keeps_mtime() {
    // Finder and Quick Look stamp "last opened" through an atime-only
    // setattr; it must not pass for a modification.
    let test = init_repo();
    common::commit_file_at(&test.repo, "a.txt", b"v1", 1_600_000_000, "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    let attr = fs
        .apply_setattr(ino, None, None, Some(epoch_time(1_800_000_000)), None)
        .unwrap();
    assert_eq!(attr.mtime, epoch_time(1_600_000_000), "setattr reply");
    assert_eq!(
        fs.apply_getattr(ino).unwrap().mtime,
        epoch_time(1_600_000_000),
        "an atime-only setattr must not move the mtime"
    );

    fs.apply_setattr(ino, None, None, None, Some(epoch_time(1_700_000_000)))
        .unwrap();
    assert_eq!(
        fs.apply_getattr(ino).unwrap().mtime,
        epoch_time(1_700_000_000),
        "an explicit mtime still applies"
    );
}

// ---- Phase 2.5, Block E: content shared between buffer and queue ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn flush_shares_buffer_arc_with_queued_op() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, _) = fs.apply_create(1, "a.txt").unwrap();
    fs.apply_write(ino, 0, b"dados").unwrap();
    fs.apply_flush(ino);

    let buf = fs.buffer_arc_for_test(ino).expect("buffer alive after flush");
    let snap = fs.sync_snapshot_for_test();
    let shared = snap.iter().any(|op| matches!(op,
        git_drive::sync::SyncOp::Write { content: git_drive::sync::WriteContent::Mem(arc), .. }
            if std::sync::Arc::ptr_eq(arc, &buf)));
    assert!(shared, "the enqueued op must share the buffer's Arc: {:?}", snap);
}

// ---- Phase 2.5, Block E: spooling large write buffers to disk ----

fn new_fs_with_spool(repo_path: &str, threshold_bytes: u64) -> GitDriveFS {
    let sync = SyncManager::new(repo_path, 5);
    GitDriveFS::new_with_options(
        repo_path,
        "main".to_string(),
        sync,
        64 * 1024 * 1024,
        threshold_bytes,
    )
    .expect("GitDriveFS::new_with_options")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn write_buffer_spills_to_disk_above_threshold() {
    let test = init_repo();
    let mut fs = new_fs_with_spool(test.dir.path().to_str().unwrap(), 8);

    let (ino, _) = fs.apply_create(1, "grande.bin").unwrap();
    fs.apply_write(ino, 0, b"0123456789ABCDEF").unwrap();
    assert!(
        fs.buffer_is_spooled_for_test(ino),
        "a buffer above the threshold must live on disk, not in RAM"
    );
    assert_eq!(
        fs.apply_read(ino, 0, 100).unwrap(),
        b"0123456789ABCDEF".to_vec(),
        "reading the spooled buffer must be identical"
    );
    assert_eq!(fs.apply_read(ino, 4, 4).unwrap(), b"4567".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn buffer_crossing_threshold_migrates_without_loss() {
    let test = init_repo();
    let mut fs = new_fs_with_spool(test.dir.path().to_str().unwrap(), 8);

    let (ino, _) = fs.apply_create(1, "cresce.bin").unwrap();
    fs.apply_write(ino, 0, b"abcd").unwrap();
    assert!(!fs.buffer_is_spooled_for_test(ino), "below the threshold it stays in RAM");
    fs.apply_write(ino, 4, b"efghijkl").unwrap();
    assert!(fs.buffer_is_spooled_for_test(ino), "crossed the threshold: migrates to spool");
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"abcdefghijkl".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ensure_buffer_of_large_existing_file_spools_head_copy() {
    // Phase 0 invariant preserved in the spooled regime: the buffer of an
    // existing file is NEVER born empty — a copy of HEAD, now on disk.
    let test = init_repo();
    commit_file(&test.repo, "big.txt", b"conteudo-original-grande", "add big");
    let mut fs = new_fs_with_spool(test.dir.path().to_str().unwrap(), 8);

    let ino = fs.find_inode("big.txt").unwrap();
    // Partial write in the middle: the rest of the file has to survive
    fs.apply_write(ino, 9, b"MUDADO--").unwrap();
    assert!(fs.buffer_is_spooled_for_test(ino));
    assert_eq!(
        fs.apply_read(ino, 0, 100).unwrap(),
        b"conteudo-MUDADO---grande".to_vec()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn truncate_on_spooled_buffer_sets_length() {
    let test = init_repo();
    let mut fs = new_fs_with_spool(test.dir.path().to_str().unwrap(), 8);
    let (ino, _) = fs.apply_create(1, "t.bin").unwrap();
    fs.apply_write(ino, 0, b"0123456789ABCDEF").unwrap();
    fs.apply_truncate(ino, 4).unwrap();
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"0123".to_vec());
    assert_eq!(fs.apply_getattr(ino).unwrap().size, 4);
}

// ---- Phase 2.5, Block E: persistent repo handle on the read path ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn read_miss_reuses_persistent_repo_handle() {
    let test = init_repo();
    commit_file(&test.repo, "big.txt", b"0123456789", "add big");
    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    // Read cache DISABLED (budget 0): every read is a miss and hits the repo
    let mut fs = GitDriveFS::new_with_options(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
        0,
        64 * 1024 * 1024,
    )
    .expect("fs");

    let ino = fs.find_inode("big.txt").unwrap();
    assert_eq!(fs.apply_read(ino, 0, 4).unwrap(), b"0123".to_vec());
    assert_eq!(fs.apply_read(ino, 4, 4).unwrap(), b"4567".to_vec());
    assert_eq!(fs.apply_read(ino, 8, 2).unwrap(), b"89".to_vec());
    assert_eq!(
        fs.read_repo_opens_for_test(),
        1,
        "the repo must be opened ONCE for N read syscalls, not once per syscall"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn persistent_read_handle_sees_new_head_after_refresh() {
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"versao-1", "v1");
    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new_with_options(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
        0,
        64 * 1024 * 1024,
    )
    .expect("fs");

    let ino = fs.find_inode("a.txt").unwrap();
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"versao-1".to_vec());

    commit_file(&test.repo, "a.txt", b"versao-2", "v2");
    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();
    assert_eq!(
        fs.apply_read(ino, 0, 100).unwrap(),
        b"versao-2".to_vec(),
        "the persistent handle must not serve the old HEAD after refresh"
    );
}

// ---- Phase 2.6: transparent reads of chunked files ----

use git_drive::chunk::{chunk_reader_into_repo, shard_rel_path, Manifest};

/// Materializes the payload as manifest + chunks in the working tree and commits everything.
fn commit_chunked(repo: &git2::Repository, rel: &str, payload: &[u8], chunk_size: u64) -> Manifest {
    let workdir = repo.workdir().unwrap();
    let m = chunk_reader_into_repo(&mut &payload[..], chunk_size, workdir).unwrap();
    std::fs::write(workdir.join(rel), m.serialize()).unwrap();

    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "chunked", &tree, &[&parent])
        .unwrap();
    m
}

fn chunk_payload() -> Vec<u8> {
    (0..250u32).map(|i| (i % 251) as u8).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunked_file_exposes_real_size_and_reads_reassembled() {
    let test = init_repo();
    let payload = chunk_payload();
    commit_chunked(&test.repo, "big.bin", &payload, 100);
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    let ino = fs.find_inode("big.bin").expect("chunked file in the index");
    assert_eq!(
        fs.apply_getattr(ino).unwrap().size,
        250,
        "getattr must expose the REAL size, not the manifest's"
    );
    assert_eq!(fs.apply_read(ino, 0, 250).unwrap(), payload);
    assert_eq!(
        fs.apply_read(ino, 90, 20).unwrap(),
        payload[90..110].to_vec(),
        "read crossing a chunk boundary"
    );
    assert_eq!(fs.apply_read(ino, 240, 100).unwrap(), payload[240..].to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn setattr_times_on_chunked_file_reports_real_size() {
    // The kernel caches the setattr reply for the TTL: a manifest-sized
    // reply made the file shrink to a few hundred bytes (reads past it hit
    // EOF) whenever Finder or Quick Look stamped its times.
    let test = init_repo();
    commit_chunked(&test.repo, "big.bin", &chunk_payload(), 100);
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("big.bin").unwrap();

    let now = std::time::SystemTime::now();
    let attr = fs.apply_setattr(ino, None, None, Some(now), Some(now)).unwrap();
    assert_eq!(attr.size, 250, "setattr must reply with the REAL size, not the manifest's");
    let attr = fs.apply_setattr(ino, Some(0o600), None, None, None).unwrap();
    assert_eq!(attr.size, 250, "chmod reply");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn interleaved_reads_across_chunks_read_each_chunk_once() {
    // Concurrent readers (Quick Look, thumbnailer, a player jumping between
    // moov and mdat) interleave requests on different chunks. A one-slot
    // chunk cache reloaded (disk + hash + decrypt) a whole chunk on every
    // switch — 0.5 s per 80 MB chunk, with the FUSE loop blocked.
    let test = init_repo();
    let payload = chunk_payload();
    commit_chunked(&test.repo, "big.bin", &payload, 100);
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("big.bin").unwrap();

    for _ in 0..3 {
        assert_eq!(fs.apply_read(ino, 10, 5).unwrap(), payload[10..15].to_vec());
        assert_eq!(fs.apply_read(ino, 210, 5).unwrap(), payload[210..215].to_vec());
    }
    assert_eq!(
        fs.range_verifications_for_test(),
        2,
        "each chunk is read whole (verified) once, not on every switch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunk_damaged_after_first_read_is_eio() {
    let test = init_repo();
    let payload = chunk_payload();
    let m = commit_chunked(&test.repo, "big.bin", &payload, 100);
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("big.bin").unwrap();
    assert_eq!(fs.apply_read(ino, 100, 10).unwrap(), payload[100..110].to_vec());

    std::fs::write(test.dir.path().join(shard_rel_path(&m.hashes[1])), b"damaged").unwrap();
    assert_eq!(
        fs.apply_read(ino, 100, 10),
        Err(libc::EIO),
        "a chunk changed on disk after its check is checked again"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunks_dir_is_invisible_in_mount() {
    let test = init_repo();
    commit_chunked(&test.repo, "big.bin", &chunk_payload(), 100);
    let fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    assert_eq!(fs.find_inode(".chunks"), None, ".chunks is an internal artifact");
    let entries = fs.apply_readdir(1).unwrap();
    assert!(
        !entries.iter().any(|(_, _, n)| n == ".chunks"),
        "readdir must not list .chunks: {:?}",
        entries
    );
    assert_eq!(fs.file_count(), 1, "only big.bin (chunks out of the index)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn corrupted_chunk_read_is_eio() {
    let test = init_repo();
    let payload = chunk_payload();
    let m = commit_chunked(&test.repo, "big.bin", &payload, 100);
    // Corrupt the second chunk in the working tree
    let victim = test.dir.path().join(shard_rel_path(&m.hashes[1]));
    std::fs::write(&victim, b"corrompido").unwrap();

    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("big.bin").unwrap();
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), payload[..100].to_vec(), "intact chunk reads normally");
    assert_eq!(
        fs.apply_read(ino, 100, 100),
        Err(libc::EIO),
        "a hash mismatch must become EIO, never wrong content"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn write_to_chunked_file_roundtrip_via_buffer() {
    let test = init_repo();
    let payload = chunk_payload();
    commit_chunked(&test.repo, "big.bin", &payload, 100);
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("big.bin").unwrap();

    // Partial write: the buffer is born from the REASSEMBLY (never from the raw manifest)
    fs.apply_write(ino, 10, b"XYZ").unwrap();
    let mut expected = payload.clone();
    expected[10..13].copy_from_slice(b"XYZ");
    assert_eq!(fs.apply_read(ino, 0, 250).unwrap(), expected);

    let flushed = fs.apply_flush(ino);
    assert!(flushed.is_some());
    let snap = fs.sync_snapshot_for_test();
    let content_len = snap
        .iter()
        .find_map(|op| match op {
            git_drive::sync::SyncOp::Write { path, content } if path == "big.bin" => {
                Some(content.to_vec().len())
            }
            _ => None,
        })
        .expect("write of big.bin in the queue");
    assert_eq!(content_len, 250, "the queue carries the modified expanded content");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn rename_chunked_file_queues_manifest_not_expanded_content() {
    let test = init_repo();
    commit_chunked(&test.repo, "big.bin", &chunk_payload(), 100);
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("big.bin").unwrap();
    // Detection happens on first access
    let _ = fs.apply_getattr(ino).unwrap();

    fs.apply_rename(1, "big.bin", 1, "renomeado.bin").unwrap();
    let snap = fs.sync_snapshot_for_test();
    let write_bytes = snap
        .iter()
        .find_map(|op| match op {
            git_drive::sync::SyncOp::Write { path, content } if path == "renomeado.bin" => {
                Some(content.to_vec())
            }
            _ => None,
        })
        .expect("write of the new path in the queue");
    assert!(
        Manifest::is_manifest_bytes(&write_bytes),
        "renaming a chunked file enqueues the MANIFEST (small), not the expanded content"
    );
    assert!(snap.iter().any(|op| matches!(op,
        git_drive::sync::SyncOp::Delete { path } if path == "big.bin")));

    // And the new path stays readable (chunks intact)
    let ino2 = fs.find_inode("renomeado.bin").unwrap();
    assert_eq!(fs.apply_read(ino2, 0, 250).unwrap(), chunk_payload());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunked_view_survives_refresh() {
    let test = init_repo();
    let payload = chunk_payload();
    commit_chunked(&test.repo, "big.bin", &payload, 100);
    let sync = SyncManager::new(test.dir.path().to_str().unwrap(), 5);
    let sync_handle = sync.clone();
    let mut fs = GitDriveFS::new(test.dir.path().to_str().unwrap(), "main".to_string(), sync)
        .expect("GitDriveFS::new");
    let ino = fs.find_inode("big.bin").unwrap();
    assert_eq!(fs.apply_getattr(ino).unwrap().size, 250);

    sync_handle.raise_refresh_flag();
    fs.refresh_if_needed();
    assert_eq!(fs.find_inode("big.bin"), Some(ino));
    assert_eq!(fs.apply_getattr(ino).unwrap().size, 250);
    assert_eq!(fs.apply_read(ino, 0, 250).unwrap(), payload);
    assert_eq!(fs.find_inode(".chunks"), None, ".chunks stays invisible after refresh");
}

// ---- Phase 2.7: cleartext FUSE view over an encrypted repo ----

use git_drive::crypt::Crypto;
use std::sync::Arc as StdArc;

fn fs_crypto() -> StdArc<Crypto> {
    StdArc::new(Crypto::from_master_key(&[42u8; 32]))
}

fn commit_all(repo: &git2::Repository, msg: &str) {
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = git2::Signature::now("Test User", "test@example.com").unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &[&parent])
        .unwrap();
}

/// Materializes an encrypted file (name + content + long-name sidecars) in the
/// worktree and commits it — mirrors what the drain's apply path produces.
fn commit_encrypted_file(
    repo: &git2::Repository,
    crypto: &Crypto,
    clear_rel: &str,
    content: &[u8],
) {
    let workdir = repo.workdir().unwrap();
    let enc = crypto.enc_path_full(clear_rel);
    let full = workdir.join(&enc.disk);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, crypto.seal(content)).unwrap();
    for sc in &enc.sidecars {
        std::fs::write(workdir.join(&sc.path), sc.content.as_bytes()).unwrap();
    }
    commit_all(repo, "add encrypted");
}

fn new_encrypted_fs(repo_path: &str, crypto: StdArc<Crypto>) -> GitDriveFS {
    let sync = SyncManager::new(repo_path, 5);
    GitDriveFS::new_with_options_crypto(
        repo_path,
        "main".to_string(),
        sync,
        64 * 1024 * 1024,
        64 * 1024 * 1024,
        Some(crypto),
    )
    .expect("encrypted GitDriveFS")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_repo_boot_exposes_clear_names_and_content() {
    let test = init_repo();
    let crypto = fs_crypto();
    commit_encrypted_file(&test.repo, &crypto, "docs/Nota Secreta.txt", b"segredo");
    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto.clone());

    let ino = fs
        .find_inode("docs/Nota Secreta.txt")
        .expect("the CLEAR name must be in the index");
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"segredo".to_vec());
    assert_eq!(
        fs.apply_getattr(ino).unwrap().size,
        7,
        "PLAINTEXT size, not the sealed blob's"
    );
    // The encrypted name does not leak into the view
    let cipher_rel = crypto.enc_path("docs/Nota Secreta.txt");
    assert_eq!(fs.find_inode(&cipher_rel), None);
    let entries = fs.apply_readdir(1).unwrap();
    assert!(entries.iter().any(|(_, _, n)| n == "docs"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_read_with_wrong_key_is_eio() {
    let test = init_repo();
    let crypto = fs_crypto();
    commit_encrypted_file(&test.repo, &crypto, "dado.bin", b"conteudo");
    let wrong = StdArc::new(Crypto::from_master_key(&[99u8; 32]));
    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), wrong);

    // The name does not decrypt with the wrong key -> shows up literal (encrypted)
    let cipher_rel = crypto.enc_path("dado.bin");
    let ino = fs.find_inode(&cipher_rel).expect("literal name fallback");
    assert_eq!(
        fs.apply_read(ino, 0, 100),
        Err(libc::EIO),
        "wrong key = EIO, never wrong content"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_symlink_readlink_returns_clear_target() {
    let test = init_repo();
    let crypto = fs_crypto();
    let workdir = test.repo.workdir().unwrap();
    let link_rel = crypto.enc_path("atalho.txt");
    // Target components above the threshold are hashed on disk; readlink
    // resolves them through the target's worktree sidecars — materialize them
    // as the drain would when the target path exists on the drive.
    let target = crypto.enc_path_full("docs/alvo real.txt");
    for sc in &target.sidecars {
        let p = workdir.join(&sc.path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, sc.content.as_bytes()).unwrap();
    }
    std::os::unix::fs::symlink(&target.disk, workdir.join(&link_rel)).unwrap();
    commit_all(&test.repo, "add symlink cifrado");

    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto);
    let ino = fs.find_inode("atalho.txt").expect("symlink with clear name");
    assert_eq!(fs.apply_getattr(ino).unwrap().kind, FileType::Symlink);
    assert_eq!(
        fs.apply_readlink(ino).unwrap(),
        b"docs/alvo real.txt".to_vec(),
        "readlink presents the CLEAR target"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_chunked_file_reads_reassembled_plaintext() {
    let test = init_repo();
    let crypto = fs_crypto();
    let workdir = test.repo.workdir().unwrap();
    let payload: Vec<u8> = (0..250u32).map(|i| (i % 251) as u8).collect();
    let seal = |plain: &[u8]| crypto.seal(plain);
    let manifest = git_drive::chunk::chunk_reader_into_repo_with(
        &mut payload.as_slice(),
        100,
        workdir,
        Some(&seal as &dyn Fn(&[u8]) -> Vec<u8>),
    )
    .unwrap();
    let cipher_rel = crypto.enc_path("grande.bin");
    std::fs::write(workdir.join(&cipher_rel), crypto.seal(&manifest.serialize())).unwrap();
    commit_all(&test.repo, "add chunked cifrado");

    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto.clone());
    let ino = fs.find_inode("grande.bin").expect("chunked file with clear name");
    assert_eq!(fs.apply_getattr(ino).unwrap().size, 250, "real plaintext size");
    assert_eq!(fs.apply_read(ino, 0, 250).unwrap(), payload);
    assert_eq!(fs.apply_read(ino, 90, 20).unwrap(), payload[90..110].to_vec());

    // Tampered chunk: the ciphertext hash detects it before decrypting
    let victim = workdir.join(shard_rel_path(&manifest.hashes[1]));
    std::fs::write(&victim, b"lixo").unwrap();
    // A fresh mount checks the chunk on its first read:
    let mut fs2 = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto);
    let ino2 = fs2.find_inode("grande.bin").unwrap();
    assert_eq!(fs2.apply_read(ino2, 100, 100), Err(libc::EIO));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_manifest_with_cleartext_chunks_reads_correctly() {
    // Mixed state mid-migration: sealed manifest, chunks still in the clear.
    let test = init_repo();
    let crypto = fs_crypto();
    let workdir = test.repo.workdir().unwrap();
    let payload: Vec<u8> = (0..250u32).map(|i| (i % 251) as u8).collect();
    let manifest = chunk_reader_into_repo(&mut payload.as_slice(), 100, workdir).unwrap();
    let cipher_rel = crypto.enc_path("misto.bin");
    std::fs::write(workdir.join(&cipher_rel), crypto.seal(&manifest.serialize())).unwrap();
    commit_all(&test.repo, "add misto");

    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto);
    let ino = fs.find_inode("misto.bin").unwrap();
    assert_eq!(fs.apply_getattr(ino).unwrap().size, 250);
    assert_eq!(fs.apply_read(ino, 90, 20).unwrap(), payload[90..110].to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_mode_disables_local_only_ignore() {
    let test = init_repo();
    let crypto = fs_crypto();
    commit_encrypted_file(&test.repo, &crypto, "base.txt", b"x");
    // clear .gitignore + a file matching the rule, straight in the worktree
    std::fs::write(test.dir.path().join(".gitignore"), b"*.log\n").unwrap();
    std::fs::write(test.dir.path().join("solto.log"), b"local").unwrap();

    let fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto);
    assert_eq!(
        fs.find_inode("solto.log"),
        None,
        "encrypted mode disables local-only incorporation"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_conflicted_copy_shows_clear_prefix() {
    let test = init_repo();
    let crypto = fs_crypto();
    let workdir = test.repo.workdir().unwrap();
    let cipher = crypto.enc_path("nota.txt");
    let conflicted_rel = format!("{}.conflict-mac-99", cipher);
    std::fs::write(workdir.join(&conflicted_rel), crypto.seal(b"versao local")).unwrap();
    commit_all(&test.repo, "conflicted copy");

    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto);
    let ino = fs
        .find_inode("nota.txt.conflict-mac-99")
        .expect("decrypted prefix + clear suffix");
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"versao local".to_vec());
}

// ---- Range reads of large files from the working tree ----

/// Above the range-read threshold (4 MiB).
fn large_payload() -> Vec<u8> {
    (0..5 * 1024 * 1024u32).map(|i| (i.wrapping_mul(131) + 17) as u8).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn large_committed_file_reads_by_range_after_one_check() {
    // Above the blob cache budget, every read used to load and inflate the
    // whole blob from the object store (21.5 s per MiB on a 70 MB video).
    let test = init_repo();
    let payload = large_payload();
    commit_file(&test.repo, "video.mp4", &payload, "add video");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("video.mp4").unwrap();

    for off in [0usize, 4_000_000, 1_000, 5 * 1024 * 1024 - 10] {
        assert_eq!(
            fs.apply_read(ino, off, 64 * 1024).unwrap(),
            payload[off..(off + 64 * 1024).min(payload.len())].to_vec()
        );
    }
    assert_eq!(fs.range_verifications_for_test(), 1, "checked once, then read by range");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn large_file_diverging_from_head_serves_head_content() {
    // Working tree edited behind the drive's back (or mid-checkout): the
    // range read must not serve it as the committed version.
    let test = init_repo();
    let payload = large_payload();
    commit_file(&test.repo, "video.mp4", &payload, "add video");
    std::fs::write(test.dir.path().join("video.mp4"), vec![0u8; payload.len()]).unwrap();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("video.mp4").unwrap();
    assert_eq!(fs.apply_read(ino, 1000, 100).unwrap(), payload[1000..1100].to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_large_file_reads_plaintext_by_range() {
    let test = init_repo();
    let crypto = fs_crypto();
    let payload = large_payload();
    commit_encrypted_file(&test.repo, &crypto, "aulas/video.mp4", &payload);
    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto);
    let ino = fs.find_inode("aulas/video.mp4").unwrap();

    assert_eq!(fs.apply_getattr(ino).unwrap().size, payload.len() as u64);
    for off in [3_000_000usize, 17, 5 * 1024 * 1024 - 100] {
        assert_eq!(
            fs.apply_read(ino, off, 4096).unwrap(),
            payload[off..(off + 4096).min(payload.len())].to_vec()
        );
    }
    assert_eq!(fs.range_verifications_for_test(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn encrypted_large_file_with_bad_tag_is_eio() {
    // A tampered blob committed (e.g. pushed to the remote by someone without
    // the key) matches its own blob id; the tag check must still refuse it.
    let test = init_repo();
    let crypto = fs_crypto();
    let workdir = test.repo.workdir().unwrap();
    let enc = crypto.enc_path_full("video.mp4");
    let mut sealed = crypto.seal(&large_payload());
    sealed[1_000_000] ^= 0x01;
    std::fs::write(workdir.join(&enc.disk), &sealed).unwrap();
    for sc in &enc.sidecars {
        std::fs::write(workdir.join(&sc.path), sc.content.as_bytes()).unwrap();
    }
    commit_all(&test.repo, "add tampered");

    let mut fs = new_encrypted_fs(test.dir.path().to_str().unwrap(), crypto);
    let ino = fs.find_inode("video.mp4").unwrap();
    assert_eq!(fs.apply_read(ino, 0, 100), Err(libc::EIO));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn large_local_only_file_reads_by_range() {
    let test = init_repo();
    commit_file(&test.repo, ".gitignore", b"*.iso\n", "add gitignore");
    let payload = large_payload();
    std::fs::write(test.dir.path().join("disco.iso"), &payload).unwrap();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("disco.iso").unwrap();
    assert_eq!(fs.apply_read(ino, 2_000_000, 300).unwrap(), payload[2_000_000..2_000_300].to_vec());
    assert_eq!(fs.range_verifications_for_test(), 0, "local-only content has nothing to check against");
}
