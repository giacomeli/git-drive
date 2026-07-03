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


#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn refresh_preserves_next_inode_when_paths_unchanged() {
    // Regressão RF7 (Fase 0): `refresh_if_needed` usava
    // `previous.get(...).unwrap_or(&self.alloc_inode())`, que avalia
    // `alloc_inode()` SEMPRE (argumento eager) — o contador de inodes saltava
    // o total de paths a cada refresh, mesmo sem nenhuma mudança.
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
        "refresh com paths inalterados não deve alocar inodes novos"
    );
    // Inodes existentes preservados
    assert_eq!(fs.find_inode("foo"), Some(ino_foo));
    assert_eq!(fs.find_inode("foo/bar/baz.txt"), Some(ino_baz));
}

// ---- Regressão RF1 (Fase 0): buffer de escrita inicializado com conteúdo corrente ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_write_append_preserves_existing_content() {
    // Antes do fix, o buffer nascia vazio e era zero-preenchido até o offset:
    // um append (`echo x >> arquivo`) substituía o conteúdo original por zeros.
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
    // Antes do fix, truncate de arquivo sem buffer criava buffer vazio e
    // resize(N, 0) — o conteúdo virava N bytes de zeros.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello world", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_truncate(ino, 4).unwrap();
    assert_eq!(
        fs.read_content_for_test(ino).as_deref(),
        Some(b"hell".as_slice())
    );
    // O truncate materializa a op imediatamente (pode não haver flush depois)
    let ops = fs.sync_snapshot_for_test();
    assert!(
        ops.iter().any(|op| matches!(op,
            drive_sync::sync::SyncOp::Write { path, content }
                if path == "a.txt" && content.as_slice() == b"hell"
        )),
        "esperado Write(a.txt) truncado, obtido {:?}",
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

// ---- Regressão RF2 (Fase 0): flush só enfileira escrita real (dirty flag) ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn flush_without_write_does_not_enqueue() {
    // flush roda em todo close(2), inclusive de fd aberto só para leitura.
    // Antes do fix, qualquer buffer existente era re-enfileirado — podendo
    // reverter mudanças remotas mergeadas com conteúdo antigo.
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
            drive_sync::sync::SyncOp::Write { path, content }
                if path == "a.txt" && content.as_slice() == b"hello world"
        )),
        "esperado Write(a.txt) com conteúdo completo, obtido {:?}",
        ops
    );

    // Segundo flush sem escrita nova (ex.: close de leitura) não re-enfileira
    assert!(fs.apply_flush(ino).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn truncate_clears_dirty_mark() {
    // apply_truncate já materializa a op — o flush seguinte não deve duplicar.
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
    // O rename de arquivo enfileira o conteúdo corrente (Write(dest)+Delete(src));
    // o flush posterior do mesmo inode não deve enfileirar de novo.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_write(ino, 5, b"!").unwrap();
    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    assert!(fs.apply_flush(ino).is_none());
}

// ---- Fase 1 RF1: cache de blobs e leitura sem HEAD-miss ----

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
    assert_eq!((hits, misses), (1, 1), "segunda leitura deve ser hit");
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
    assert_eq!(fs.read_cache_len_for_test(), 0, "refresh deve limpar o cache");
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
        6, // cabe só um blob de 4 bytes por vez
    )
    .expect("GitDriveFS::new_with_cache_budget");
    let ino_a = fs.find_inode("a.txt").unwrap();
    let ino_b = fs.find_inode("b.txt").unwrap();

    fs.apply_read(ino_a, 0, 100).unwrap();
    fs.apply_read(ino_b, 0, 100).unwrap();
    assert_eq!(fs.read_cache_len_for_test(), 1, "orçamento força eviction");
    assert!(fs.read_cache_contains_for_test(ino_b));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_read_after_create_returns_empty_not_eio() {
    // Regressão do gap descoberto na Fase 1: o path recém-criado não existe no
    // HEAD — sem o buffer vazio, read retornava EIO até o primeiro commit.
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, _) = fs.apply_create(1, "novo.txt").unwrap();

    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn apply_read_after_rename_returns_content_not_eio() {
    // Mesmo gap para rename: o path NOVO não existe no HEAD até o commit.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let ino = fs.find_inode("a.txt").unwrap();

    fs.apply_rename(1, "a.txt", 1, "b.txt").unwrap();
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"hello");
}

// ---- Fase 1 RF2: validação do swap implícito ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn implicit_swap_with_two_files_in_source_returns_exdev() {
    // O contrato do safe-save é um temp dir com UM arquivo. Com dois, escolher
    // qualquer um é chute — antes o código pegava o primeiro da iteração do
    // HashSet e podia trocar o conteúdo do arquivo errado.
    let test = init_repo();
    commit_file(&test.repo, "tmpdir/a.txt", b"aaa", "add a");
    commit_file(&test.repo, "tmpdir/b.txt", b"bbb", "add b");
    commit_file(&test.repo, "a.txt", b"root-a", "add root a");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let tmpdir_ino = fs.find_inode("tmpdir").unwrap();
    let root_a = fs.find_inode("a.txt").unwrap();

    // RENAME com nomes vazios = encoding do exchangedata no macFUSE
    let result = fs.apply_rename(tmpdir_ino, "", 1, "");
    assert_eq!(result, Err(libc::EXDEV));

    // Nenhum conteúdo alterado, nenhuma op enfileirada
    assert_eq!(fs.read_content_for_test(root_a).as_deref(), Some(b"root-a".as_slice()));
    assert!(fs.sync_snapshot_for_test().is_empty());
}

// ---- Fase 1 RF3: renames envolvendo paths efêmeros ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn dir_rename_to_ephemeral_queues_recursive_removal() {
    // Antes, o filtro descartava o Rename inteiro: o disco mantinha o dir e o
    // índice em memória dizia que ele tinha virado efêmero — divergência.
    let test = init_repo();
    commit_file(&test.repo, "d/f.txt", b"conteudo-f", "add f");
    commit_file(&test.repo, "d/sub/g.txt", b"conteudo-g", "add g");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let f_ino = fs.find_inode("d/f.txt").unwrap();

    fs.apply_rename(1, "d", 1, "._d").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use drive_sync::sync::SyncOp;
    assert!(
        !ops.iter().any(|op| matches!(op, SyncOp::Rename { .. })),
        "não deve haver Rename na fila, obtido {:?}",
        ops
    );
    assert!(ops.iter().any(|op| matches!(op, SyncOp::Delete { path } if path == "d/f.txt")));
    assert!(ops.iter().any(|op| matches!(op, SyncOp::Delete { path } if path == "d/sub/g.txt")));
    let pos_sub = ops.iter().position(|op| matches!(op, SyncOp::Rmdir { path } if path == "d/sub"));
    let pos_root = ops.iter().position(|op| matches!(op, SyncOp::Rmdir { path } if path == "d"));
    assert!(pos_sub.is_some() && pos_root.is_some(), "rmdirs esperados, obtido {:?}", ops);
    assert!(pos_sub < pos_root, "rmdir deve ser bottom-up");

    // Conteúdo continua legível dentro da área efêmera (Trash)
    assert_eq!(fs.apply_read(f_ino, 0, 100).unwrap(), b"conteudo-f");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn dir_rename_from_ephemeral_materializes_content() {
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    // Dir efêmero construído em memória (todas as ops são filtradas da fila)
    let (dir_ino, _) = fs.apply_mkdir(1, "._hidden").unwrap();
    let (f_ino, _) = fs.apply_create(dir_ino, "f.txt").unwrap();
    fs.apply_write(f_ino, 0, b"segredo").unwrap();
    fs.apply_flush(f_ino);
    assert!(fs.sync_snapshot_for_test().is_empty(), "ops efêmeras devem ter sido filtradas");

    fs.apply_rename(1, "._hidden", 1, "visible").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use drive_sync::sync::SyncOp;
    let pos_mkdir = ops.iter().position(|op| matches!(op, SyncOp::Mkdir { path } if path == "visible"));
    let pos_write = ops.iter().position(|op| matches!(op,
        SyncOp::Write { path, content } if path == "visible/f.txt" && content.as_slice() == b"segredo"));
    assert!(pos_mkdir.is_some(), "mkdir esperado, obtido {:?}", ops);
    assert!(pos_write.is_some(), "write com conteúdo esperado, obtido {:?}", ops);
    assert!(pos_mkdir < pos_write, "mkdir deve preceder o write");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn file_rename_to_ephemeral_queues_only_delete() {
    // Regressão do comportamento atual (correto): Write(destino efêmero) é
    // filtrado, Delete(origem real) passa — o arquivo some do repo.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"hello", "add");
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");

    fs.apply_rename(1, "a.txt", 1, "._a.txt").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use drive_sync::sync::SyncOp;
    assert_eq!(ops.len(), 1, "esperado só o Delete, obtido {:?}", ops);
    assert!(matches!(&ops[0], SyncOp::Delete { path } if path == "a.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn file_rename_from_ephemeral_materializes_write() {
    // Regressão do comportamento atual (correto): Write(destino real) passa
    // com o conteúdo corrente; Delete(origem efêmera) é filtrado.
    let test = init_repo();
    let mut fs = new_fs(test.dir.path().to_str().unwrap(), "main");
    let (ino, _) = fs.apply_create(1, "._tmp.txt").unwrap();
    fs.apply_write(ino, 0, b"data").unwrap();
    fs.apply_flush(ino);
    assert!(fs.sync_snapshot_for_test().is_empty());

    fs.apply_rename(1, "._tmp.txt", 1, "real.txt").unwrap();

    let ops = fs.sync_snapshot_for_test();
    use drive_sync::sync::SyncOp;
    assert_eq!(ops.len(), 1, "esperado só o Write, obtido {:?}", ops);
    assert!(matches!(&ops[0], SyncOp::Write { path, content }
        if path == "real.txt" && content.as_slice() == b"data"));
}
