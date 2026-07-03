mod common;

use common::{commit_file, file_exists, init_bare_remote, init_repo, read_file};
use drive_sync::sync::SyncManager;
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
    assert!(msg.starts_with("drive-sync:"), "msg={}", msg);
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
    // Preparar: dir existe antes do SyncManager começar
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
        "nenhum commit deveria ter sido criado"
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
    // parent deve ser o commit inicial (único commit novo)
    assert_eq!(parent.parents().count(), 0);
    let msg = head.message().unwrap();
    assert!(msg.contains("3 changes") || msg.contains("3 mudanças"), "msg={}", msg);
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
    // publicar commit inicial no remote
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    // Iniciar o SyncManager com pull_interval baixo
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

    // Outro clone (B) empurra commit novo
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
    drive_sync::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    // esperar pelo menos 2 ciclos de pull (1s cada)
    tokio::time::sleep(Duration::from_millis(3500)).await;

    assert!(sync.take_refresh_flag(), "refresh_flag não foi disparado");
    // o arquivo deve estar no working tree de A após o fast-forward
    assert!(file_exists(test.dir.path(), "from_b.txt"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn refresh_if_needed_rebuilds_index_after_pull() {
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    let sync = drive_sync::sync::SyncManager::new_full_with_pull(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        60,
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
        Some("origin".to_string()),
        None,
        1,
    );
    let mut fs = drive_sync::fs::GitDriveFS::new(
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
    drive_sync::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    tokio::time::sleep(Duration::from_millis(3500)).await;

    fs.refresh_if_needed();
    assert!(fs.find_inode("from_b.txt").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_delete_nonexistent_sb_path_is_idempotent() {
    // Regressão para o bug do Finder paste:
    // o filtro de `op_touches_ephemeral` bloqueia Write/Mkdir em paths `.sb-*`
    // (safe-save do TextEdit/NSDocument) mas deixa passar Delete/Rmdir, então
    // a sequência típica do Finder enfileira remoções de paths que nunca
    // chegaram ao disco. Antes do fix, isso virava `eprintln!` ENOENT no log
    // ("disk: erro ao remover ..."). Agora deve ser silencioso.
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

    // Nada mudou no working tree, então não deve haver commit novo.
    let head_after = test.repo.head().unwrap().peel_to_commit().unwrap().id();
    assert_eq!(
        head_before, head_after,
        "nenhum commit deveria ter sido criado"
    );

    // Buffer foi drenado (ops foram processadas, mesmo que NotFound).
    assert!(
        sync.buffer_snapshot().is_empty(),
        "buffer deveria estar vazio após drain"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_delete_existing_file_still_commits() {
    // Garantia de que o fix de "NotFound silencioso" não quebrou o caminho
    // feliz: deletar um arquivo que existe ainda gera commit normal.
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
        "commit deveria mencionar a.txt"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn sync_flow_push_failure_does_not_rollback_commit() {
    let test = init_repo();
    // remote inválido — push vai falhar
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

    // commit local deve ter sido criado mesmo com push falhando
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    assert!(head.message().unwrap().contains("x.txt"));
}

// ---- Regressão RF3 (Fase 0): invalidação de write buffers ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn clean_buffers_dropped_after_local_commit_dirty_survives() {
    // Buffers "limpos" (conteúdo já enfileirado e commitado) devem ser
    // descartados quando a commit_generation avança; buffers dirty (escrita
    // pendente sem flush) devem sobreviver intactos.
    let test = init_repo();
    commit_file(&test.repo, "a.txt", b"old", "add a");
    commit_file(&test.repo, "b.txt", b"old-b", "add b");

    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    let mut fs = drive_sync::fs::GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();
    let ino_a = fs.find_inode("a.txt").unwrap();
    let ino_b = fs.find_inode("b.txt").unwrap();

    // a.txt: escrita completa + flush (segue para drain + commit)
    fs.apply_write(ino_a, 0, b"new").unwrap();
    fs.apply_flush(ino_a).expect("flush de a.txt deve enfileirar");
    // b.txt: escrita pendente sem flush (dirty)
    fs.apply_write(ino_b, 0, b"pending").unwrap();

    wait_flush().await; // drain aplica + commita (commit_generation avança)

    fs.refresh_if_needed();

    assert_eq!(
        fs.buffered_count_for_test(),
        1,
        "apenas o buffer dirty (b.txt) deve sobreviver ao gc"
    );
    assert_eq!(
        fs.read_content_for_test(ino_a).as_deref(),
        Some(b"new".as_slice()),
        "a.txt deve vir do HEAD commitado"
    );
    assert_eq!(
        fs.read_content_for_test(ino_b).as_deref(),
        Some(b"pending".as_slice()),
        "b.txt deve continuar vindo do buffer dirty"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn remote_update_visible_after_refresh_despite_local_buffer() {
    // Cenário-título do bug RF3: depois de uma escrita local, o buffer ficava
    // vivo para sempre e o read nunca mais enxergava mudanças remotas naquele
    // arquivo (buffer tem precedência sobre o HEAD).
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    commit_file(&test.repo, "hi.txt", b"local-v1", "add hi");
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

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
    let mut fs = drive_sync::fs::GitDriveFS::new(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        sync,
    )
    .unwrap();
    let ino = fs.find_inode("hi.txt").unwrap();

    // Escrita local: v2 (cria buffer) + flush + drain + commit + push
    fs.apply_write(ino, 0, b"local-v2").unwrap();
    fs.apply_flush(ino).expect("flush deve enfileirar");
    wait_flush().await;

    // B clona (já com v2), escreve v3 e publica
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
    drive_sync::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    // esperar o pull watcher trazer o FF
    tokio::time::sleep(Duration::from_millis(3500)).await;

    fs.refresh_if_needed();
    assert_eq!(
        fs.read_content_for_test(ino).as_deref(),
        Some(b"remote-v3".as_slice()),
        "read deve refletir a versão remota após o refresh, não o buffer antigo"
    );
}
