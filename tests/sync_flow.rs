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

// ---- Regressão RF5 (Fase 0): batch com ordem causal ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn batch_chained_dir_renames_apply_in_order() {
    // Antes (HashMap + sort por tipo), dois renames no mesmo batch executavam
    // em ordem arbitrária — um falhava com ENOENT e o disco divergia do índice.
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

    assert!(!file_exists(test.dir.path(), "d1"), "d1 não deve sobrar");
    assert!(!file_exists(test.dir.path(), "d2"), "d2 não deve sobrar");
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

    assert!(!file_exists(test.dir.path(), "d2"), "d2 não deve sobrar");
    assert_eq!(read_file(test.dir.path(), "d1/f.txt"), b"x");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn batch_mkdir_rename_write_into_renamed_dir() {
    // Fluxo comum do Finder: criar pasta -> renomear -> soltar arquivo dentro,
    // tudo dentro do debounce. Antes, o Write (prioridade menor que Rename)
    // criava o destino via create_dir_all e o fs::rename falhava com
    // "destino existe e não está vazio" — o dir antigo ressuscitava.
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

    assert!(!file_exists(test.dir.path(), "na"), "dir original não deve sobrar");
    assert_eq!(read_file(test.dir.path(), "nb/f.txt"), b"conteudo");
    assert!(
        file_exists(test.dir.path(), "nb/.gitkeep"),
        ".gitkeep do mkdir deve ter viajado com o rename"
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
        "working tree líquido inalterado não deve gerar commit"
    );
}

// ---- Regressão RF6 (Fase 0): pull só com working tree limpo ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn pull_watcher_skips_when_working_tree_dirty() {
    // Cenário de perda de dados: no modo sem identidade, escritas locais vivem
    // só no working tree (commits são pulados). Antes do fix, o fast-forward
    // do pull rodava checkout force e DESTRUÍA essas mudanças.
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    commit_file(&test.repo, "hi.txt", b"v1", "add hi");
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

    // Mudança local NÃO commitada (working tree sujo)
    std::fs::write(test.dir.path().join("hi.txt"), b"local-edit").unwrap();

    // Pull watcher sem identidade (modo degradado, FF-only), intervalo 1s
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

    // B publica v2 no remote
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
    drive_sync::git::push_to_remote(&repo_b, "main", "origin", None).unwrap();

    // esperar mais de um ciclo de pull
    tokio::time::sleep(Duration::from_millis(3500)).await;

    assert_eq!(
        read_file(test.dir.path(), "hi.txt"),
        b"local-edit",
        "mudança local não commitada não pode ser destruída pelo pull"
    );
    assert!(
        !sync.take_refresh_flag(),
        "pull não deve ter aplicado nada com working tree sujo"
    );
}

// ---- Fase 1 RF4: status.json do daemon ----

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
    assert!(status_path.exists(), "enable deve gravar o estado inicial");

    sync.queue_write("a.txt", b"hello".to_vec());
    wait_flush().await;

    let status = drive_sync::status::read_from(&status_path).unwrap();
    assert_eq!(status.branch, "main");
    assert_eq!(status.pending_ops, 0);
    let drain = status.last_drain.expect("drain deve ter sido registrado");
    assert_eq!(drain.ops, 1);
    assert!(drain.commit.is_some(), "commit deve ter oid");
    assert!(drain.pushed.is_none(), "sem remote configurado, pushed é None");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn status_file_records_pull_outcome() {
    let test = init_repo();
    let bare = init_bare_remote();
    let bare_url = format!("file://{}", bare.path().to_str().unwrap());
    test.repo.remote("origin", &bare_url).unwrap();
    drive_sync::git::push_to_remote(&test.repo, "main", "origin", None).unwrap();

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

    // esperar ao menos um ciclo de pull (sem novidades no remote)
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let status = drive_sync::status::read_from(&status_path).unwrap();
    let pull = status.last_pull.expect("pull deve ter sido registrado");
    assert_eq!(pull.outcome, "sem novidades");
}

// ---- Fase 2 RF2: drain_now no shutdown ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn drain_now_flushes_pending_ops_without_waiting_debounce() {
    // Shutdown limpo: ops enfileiradas viram commit imediatamente, sem esperar
    // o debounce (que aqui é alto de propósito).
    let test = init_repo();
    let sync = SyncManager::new_with_identity(
        test.dir.path().to_str().unwrap(),
        "main".to_string(),
        3600, // debounce de 1h — o watcher nunca dispararia no teste
        Some("Alice".to_string()),
        Some("a@x.com".to_string()),
    );
    sync.queue_write("pendente.txt", b"nao pode se perder".to_vec());
    sync.queue_mkdir("dir-pendente");

    sync.drain_now();

    assert!(!sync.has_pending_ops(), "fila deve estar vazia após drain_now");
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

// ---- Fase 2.5, Bloco B: symlinks no drain e no commit ----

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
    let meta = std::fs::symlink_metadata(&link_path).expect("symlink deve existir no disco");
    assert!(meta.file_type().is_symlink(), "deve ser symlink real no working tree");
    assert_eq!(
        std::fs::read_link(&link_path).unwrap(),
        std::path::PathBuf::from("real.txt")
    );

    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    let entry = tree.get_path(std::path::Path::new("alias.txt")).unwrap();
    assert_eq!(entry.filemode(), 0o120000, "entrada da tree deve ser symlink (120000)");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn symlink_roundtrip_created_in_fs_read_back_by_new_mount() {
    use drive_sync::fs::GitDriveFS;
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

    // Novo mount lê o symlink de volta do HEAD
    let sync2 = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs2 = GitDriveFS::new(repo_path, "main".to_string(), sync2).expect("fs2");
    let ino2 = fs2.find_inode("alias.txt").expect("alias no índice do novo mount");
    assert_eq!(fs2.apply_getattr(ino2).unwrap().kind, FileType::Symlink);
    assert_eq!(fs2.apply_readlink(ino2).unwrap(), b"real.txt".to_vec());
}

// ---- Fase 2.5, Bloco A: ignorados local-only através do drain/commit ----

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ignored_file_stays_visible_and_out_of_commits() {
    use drive_sync::fs::GitDriveFS;

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

    // Commit aconteceu (real.txt) e NÃO contém o ignorado
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(tree.get_path(std::path::Path::new("real.txt")).is_ok());
    assert!(
        tree.get_path(std::path::Path::new("app.log")).is_err(),
        "app.log é ignorado e não pode aparecer em commit"
    );
    // Working tree persiste o local-only
    assert_eq!(read_file(test.dir.path(), "app.log"), b"segredo local");

    // Pós-gc (commit_generation avançou), continua visível e legível
    fs.refresh_if_needed();
    let ino = fs.find_inode("app.log").expect("visível após commit de outros arquivos");
    assert_eq!(fs.apply_read(ino, 0, 100).unwrap(), b"segredo local".to_vec());

    // Remount reincorpora do working tree
    let sync2 = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs2 = GitDriveFS::new(repo_path, "main".to_string(), sync2).expect("fs2");
    let ino2 = fs2.find_inode("app.log").expect("visível após remount");
    assert_eq!(fs2.apply_read(ino2, 0, 100).unwrap(), b"segredo local".to_vec());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn spooled_write_drains_and_commits_correct_content() {
    use drive_sync::fs::GitDriveFS;

    let test = init_repo();
    commit_file(&test.repo, "seed.txt", b"x", "seed");
    let repo_path = test.dir.path().to_str().unwrap();
    let sync = mgr(repo_path, "main", Some(("Alice", "a@x.com")));
    let mut fs = GitDriveFS::new_with_options(
        repo_path,
        "main".to_string(),
        sync,
        64 * 1024 * 1024,
        8, // limiar minúsculo: força o regime spooled
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
        "conteúdo spooled deve chegar íntegro ao working tree"
    );
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(
        tree.get_path(std::path::Path::new("video.bin")).is_ok(),
        "arquivo spooled deve ser commitado normalmente"
    );
}

// ---- Fase 2.6: chunking no drain (conversão, varredura pré-commit, GC) ----

use drive_sync::chunk::{shard_rel_path, Manifest};

/// Nenhum blob BRUTO acima do limite na tree do HEAD — manifests são isentos
/// (em escala de teste o manifest pode exceder o limiar minúsculo; em produção
/// ele é ordens de grandeza menor que o limiar).
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
    let bytes = std::fs::read(dir.join(rel)).expect("manifest deve existir no working tree");
    assert!(
        Manifest::is_manifest_bytes(&bytes),
        "arquivo deveria ser manifest, obtido: {:?}...",
        &bytes[..bytes.len().min(40)]
    );
    Manifest::parse(&bytes).expect("manifest parseável")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn chunked_write_commits_manifest_and_chunks() {
    let test = init_repo();
    let sync = mgr(
        test.dir.path().to_str().unwrap(),
        "main",
        Some(("Alice", "a@x.com")),
    );
    sync.set_chunking(64, 32); // limiar 64 bytes, chunks de 32 (teste)

    let payload: Vec<u8> = (0..200u8).collect();
    sync.queue_write("grande.bin", payload.clone());
    wait_flush().await;

    // Working tree: manifest no path, chunks íntegros em .chunks/
    let manifest = read_manifest_at(test.dir.path(), "grande.bin");
    assert_eq!(manifest.total_size, 200);
    assert_eq!(manifest.hashes.len(), 7); // ceil(200/32)
    let mut reassembled = Vec::new();
    for h in &manifest.hashes {
        reassembled.extend_from_slice(&std::fs::read(test.dir.path().join(shard_rel_path(h))).unwrap());
    }
    assert_eq!(reassembled, payload, "chunks remontados devem reproduzir o original");

    // Commit: manifest + chunks na tree, nenhum blob acima do limiar
    let head = test.repo.head().unwrap().peel_to_commit().unwrap();
    let tree = head.tree().unwrap();
    assert!(tree.get_path(std::path::Path::new("grande.bin")).is_ok());
    assert!(
        tree.get_path(&shard_rel_path(&manifest.hashes[0])).is_ok(),
        "chunks devem estar commitados"
    );
    assert!(
        tree_has_no_blob_above(&test.repo, 64),
        "nenhum blob acima do limiar pode ser stageado"
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

    // Arquivo grande chega ao working tree por FORA da fila (caso do zip do
    // incidente): a varredura pré-commit deve convertê-lo mesmo assim.
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

    // Update: conteúdo novo -> manifest novo, chunks antigos órfãos somem
    sync.queue_write("dado.bin", vec![2u8; 100]);
    wait_flush().await;
    let new = read_manifest_at(test.dir.path(), "dado.bin");
    assert_ne!(old.hashes, new.hashes);
    for h in &old.hashes {
        assert!(
            !test.dir.path().join(shard_rel_path(h)).exists(),
            "chunk órfão do manifest antigo deve ser removido do working tree"
        );
    }
    for h in &new.hashes {
        assert!(test.dir.path().join(shard_rel_path(h)).exists());
    }

    // Delete: manifest e chunks somem
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
    assert_eq!(ma.hashes, mb.hashes, "conteúdo idêntico compartilha chunks (dedup)");

    sync.queue_delete("a.bin");
    wait_flush().await;
    for h in &mb.hashes {
        assert!(
            test.dir.path().join(shard_rel_path(h)).exists(),
            "chunk ainda referenciado por b.bin não pode ser coletado"
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

    // Local-only grande fica INTACTO (não é commitado, chunkar quebraria a leitura)
    std::fs::write(test.dir.path().join("grande.log"), vec![3u8; 200]).unwrap();
    sync.queue_write("gatilho.txt", b"x".to_vec());
    wait_flush().await;

    let bytes = std::fs::read(test.dir.path().join("grande.log")).unwrap();
    assert_eq!(bytes, vec![3u8; 200], "ignorado não pode ser convertido em manifest");
}
