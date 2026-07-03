mod common;

use common::{commit_file, init_repo};
use drive_sync::fs::GitDriveFS;
use drive_sync::sync::SyncManager;
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
    let attr = fs.apply_lookup(1, "foo").expect("foo deve existir");
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
    // deve aparecer no readdir da raiz
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
    // deve haver um Write { path: "novo.txt", content: [] }
    assert!(ops.iter().any(|op| matches!(op,
        drive_sync::sync::SyncOp::Write { path, content }
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
        drive_sync::sync::SyncOp::Delete { path } if path == "a.txt"
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
        drive_sync::sync::SyncOp::Mkdir { path } if path == "pasta"
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
    // Após mkdir + rmdir na mesma chave, buffer tem apenas Rmdir (último vence).
    assert!(ops.iter().any(|op| matches!(op,
        drive_sync::sync::SyncOp::Rmdir { path } if path == "pasta"
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
    // Rename de arquivo é traduzido em Write(dest, content) + Delete(src) para
    // desacoplar de dependências de disco (o src pode nunca ter sido materializado
    // se operações anteriores foram coalescidas no buffer).
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    let ops = fs.sync_snapshot_for_test();
    assert!(
        ops.iter().any(|op| matches!(op,
            drive_sync::sync::SyncOp::Write { path, content }
                if path == "b.txt" && content.as_slice() == b"hello"
        )),
        "esperado Write(b.txt) com conteúdo do original, obtido {:?}",
        ops
    );
    assert!(
        ops.iter().any(|op| matches!(op,
            drive_sync::sync::SyncOp::Delete { path } if path == "a.txt"
        )),
        "esperado Delete(a.txt), obtido {:?}",
        ops
    );
}

