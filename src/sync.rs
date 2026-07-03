use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::{sleep, Instant};
use tokio::task;

use crate::git;
use crate::log_ts;
use crate::status::StatusState;

#[derive(Debug, Clone)]
pub enum SyncOp {
    Write { path: String, content: Vec<u8> },
    Delete { path: String },
    Mkdir { path: String },
    Rmdir { path: String },
    Rename { from: String, to: String },
}

impl SyncOp {
    pub fn primary_path(&self) -> &str {
        match self {
            SyncOp::Write { path, .. } => path,
            SyncOp::Delete { path } => path,
            SyncOp::Mkdir { path } => path,
            SyncOp::Rmdir { path } => path,
            SyncOp::Rename { to, .. } => to,
        }
    }
}

#[derive(Clone)]
pub struct SyncManager {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
    debounce_secs: u64,
    repo_path: PathBuf,
    branch: String,
    user_name: Option<String>,
    user_email: Option<String>,
    remote_name: Option<String>,
    remote_token: Option<String>,
    refresh_flag: Arc<AtomicBool>,
    /// Incrementado a cada commit local criado pelo drain. `GitDriveFS` compara
    /// com o último valor visto para descartar write buffers "limpos" (conteúdo
    /// já commitado no HEAD) — ver `refresh_if_needed`.
    commit_generation: Arc<AtomicU64>,
    /// Serializa os ciclos completos dos dois watchers sobre o mesmo repo:
    /// drain (apply + commit + push + reconcile) e pull (fetch + merge +
    /// checkout) nunca rodam concorrentes. A unidade atômica é o CICLO, não a
    /// chamada git individual — um commit entrando entre o fetch e o checkout
    /// do pull era a janela de corrida (RF4 da Fase 0).
    repo_lock: Arc<Mutex<()>>,
    /// Estado observável (status.json + subcomando `status`).
    status: Arc<StatusState>,
}

struct Inner {
    /// Fila em ordem de chegada. A ordem do kernel FUSE É a ordem causal —
    /// preservá-la elimina por construção os bugs de "rename encadeado
    /// embaralhado" e "write aplicado antes do rename do diretório" que a
    /// combinação HashMap + sort por tipo de op produzia.
    buffer: Vec<SyncOp>,
    last_op: Option<Instant>,
}

impl SyncManager {
    pub fn new(repo_path: &str, debounce_secs: u64) -> Self {
        Self::new_full(
            repo_path,
            "main".to_string(),
            debounce_secs,
            None,
            None,
            None,
            None,
        )
    }

    pub fn new_with_identity(
        repo_path: &str,
        branch: String,
        debounce_secs: u64,
        user_name: Option<String>,
        user_email: Option<String>,
    ) -> Self {
        Self::new_full(
            repo_path,
            branch,
            debounce_secs,
            user_name,
            user_email,
            None,
            None,
        )
    }

    pub fn new_full(
        repo_path: &str,
        branch: String,
        debounce_secs: u64,
        user_name: Option<String>,
        user_email: Option<String>,
        remote_name: Option<String>,
        remote_token: Option<String>,
    ) -> Self {
        Self::new_full_with_pull(
            repo_path,
            branch,
            debounce_secs,
            user_name,
            user_email,
            remote_name,
            remote_token,
            0, // 0 desliga o pull watcher
        )
    }

    pub fn new_full_with_pull(
        repo_path: &str,
        branch: String,
        debounce_secs: u64,
        user_name: Option<String>,
        user_email: Option<String>,
        remote_name: Option<String>,
        remote_token: Option<String>,
        pull_interval_secs: u64,
    ) -> Self {
        let status = Arc::new(StatusState::new());
        status.record_boot(repo_path, &branch);
        let manager = SyncManager {
            inner: Arc::new(Mutex::new(Inner {
                buffer: Vec::new(),
                last_op: None,
            })),
            notify: Arc::new(Notify::new()),
            debounce_secs,
            repo_path: PathBuf::from(repo_path),
            branch,
            user_name,
            user_email,
            remote_name,
            remote_token,
            refresh_flag: Arc::new(AtomicBool::new(false)),
            commit_generation: Arc::new(AtomicU64::new(0)),
            repo_lock: Arc::new(Mutex::new(())),
            status,
        };

        manager.spawn_watcher();
        if pull_interval_secs > 0 && manager.remote_name.is_some() {
            manager.spawn_pull_watcher(pull_interval_secs);
        }
        manager
    }

    /// Retorna o valor atual da refresh_flag e zera. Usado por `GitDriveFS`
    /// para decidir quando reconstruir os índices.
    pub fn take_refresh_flag(&self) -> bool {
        self.refresh_flag.swap(false, Ordering::SeqCst)
    }

    /// Sinaliza que os índices do FUSE devem ser reconstruídos no próximo
    /// lookup/getattr/readdir. Contraparte de `take_refresh_flag`; os watchers
    /// internos usam o mesmo mecanismo via clone do `Arc`.
    pub fn raise_refresh_flag(&self) {
        self.refresh_flag.store(true, Ordering::SeqCst);
    }

    /// Número de commits locais criados pelo drain desde o boot. Monotônico.
    pub fn commit_generation(&self) -> u64 {
        self.commit_generation.load(Ordering::SeqCst)
    }

    /// Liga a persistência do status em arquivo (grava o estado corrente).
    pub fn enable_status_file(&self, path: std::path::PathBuf) {
        self.status.enable_file(path);
    }

    /// Acesso ao estado observável (snapshot para testes/CLI embutido).
    pub fn status_state(&self) -> Arc<StatusState> {
        self.status.clone()
    }

    fn spawn_pull_watcher(&self, interval_secs: u64) {
        let inner = self.inner.clone();
        let repo_path = self.repo_path.clone();
        let branch = self.branch.clone();
        let remote_name = self.remote_name.clone();
        let remote_token = self.remote_token.clone();
        let user_name = self.user_name.clone();
        let user_email = self.user_email.clone();
        let refresh_flag = self.refresh_flag.clone();
        let repo_lock = self.repo_lock.clone();
        let status = self.status.clone();

        task::spawn(async move {
            loop {
                sleep(Duration::from_secs(interval_secs)).await;

                // Não roda pull enquanto há lote pendente (evita corrida com commit local)
                let pending = { !inner.lock().buffer.is_empty() };
                if pending {
                    status.record_pull("pulado: fila de sync pendente");
                    continue;
                }
                let Some(ref remote) = remote_name else {
                    break;
                };

                // Serializa com o ciclo do drain (RF4). Sem .await enquanto o
                // guard vive — o lock cobre só chamadas bloqueantes de git.
                let _repo_guard = repo_lock.lock();

                let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("[{}] pull: erro ao abrir repo: {}", log_ts(), e);
                        continue;
                    }
                };

                // Guarda RF6: merge/checkout force só com working tree limpo.
                // Tree suja = mudanças locais ainda não commitadas (típico do
                // modo sem identidade) — um fast-forward as destruiria.
                match git::is_working_tree_clean(&repo) {
                    Ok(true) => {}
                    Ok(false) => {
                        println!(
                            "[{}] pull: working tree com mudanças não commitadas — pulando",
                            log_ts()
                        );
                        status.record_pull("pulado: working tree sujo");
                        continue;
                    }
                    Err(e) => {
                        eprintln!("[{}] pull: erro ao checar working tree: {}", log_ts(), e);
                        status.record_pull("erro ao checar working tree");
                        continue;
                    }
                }

                if let Err(e) =
                    git::fetch_remote(&repo, &branch, remote, remote_token.as_deref())
                {
                    eprintln!("[{}] pull: erro no fetch: {}", log_ts(), e);
                    status.record_pull("erro no fetch");
                    continue;
                }

                // Com identidade: merge 3-way + conflicted copy em caso de conflito.
                // Sem identidade: degrada para FF-only (não dá pra criar merge commit).
                let outcome: Result<git::MergeOutcome, git2::Error> =
                    match (user_name.as_deref(), user_email.as_deref()) {
                        (Some(name), Some(email)) => {
                            let suffix = conflict_suffix();
                            git::pull_and_merge(&repo, &branch, remote, name, email, &suffix)
                        }
                        _ => git::merge_ff_only(&repo, &branch).map(|applied| {
                            if applied {
                                git::MergeOutcome::FastForwarded
                            } else {
                                git::MergeOutcome::NoOp
                            }
                        }),
                    };

                match outcome {
                    Ok(git::MergeOutcome::NoOp) => {
                        status.record_pull("sem novidades");
                    }
                    Ok(git::MergeOutcome::FastForwarded) => {
                        println!("[{}] pull: fast-forward aplicado", log_ts());
                        status.record_pull("fast-forward");
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Ok(git::MergeOutcome::Merged) => {
                        println!("[{}] pull: merge 3-way sem conflitos", log_ts());
                        status.record_pull("merge 3-way");
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Ok(git::MergeOutcome::Conflicted { resolved, suffix }) => {
                        println!(
                            "[{}] pull: resolveu {} conflito(s) — locais em *.{}: {:?}",
                            log_ts(),
                            resolved.len(),
                            suffix,
                            resolved
                        );
                        status.record_pull(&format!("merge com {} conflito(s)", resolved.len()));
                        status.add_conflicts(
                            resolved
                                .iter()
                                .map(|p| format!("{}.{}", p, suffix))
                                .collect(),
                        );
                        refresh_flag.store(true, Ordering::SeqCst);
                    }
                    Err(e) => {
                        eprintln!("[{}] pull: erro no merge: {}", log_ts(), e);
                        status.record_pull("erro no merge");
                    }
                }
            }
        });
    }

    pub fn queue_write(&self, path: &str, content: Vec<u8>) {
        self.enqueue(SyncOp::Write {
            path: path.to_string(),
            content,
        });
    }

    pub fn queue_delete(&self, path: &str) {
        self.enqueue(SyncOp::Delete {
            path: path.to_string(),
        });
    }

    pub fn queue_mkdir(&self, path: &str) {
        self.enqueue(SyncOp::Mkdir {
            path: path.to_string(),
        });
    }

    pub fn queue_rmdir(&self, path: &str) {
        self.enqueue(SyncOp::Rmdir {
            path: path.to_string(),
        });
    }

    pub fn queue_rename(&self, from: &str, to: &str) {
        self.enqueue(SyncOp::Rename {
            from: from.to_string(),
            to: to.to_string(),
        });
    }

    fn enqueue(&self, op: SyncOp) {
        if op_touches_ephemeral(&op) {
            return;
        }
        {
            let mut inner = self.inner.lock();
            push_coalescing(&mut inner.buffer, op);
            inner.last_op = Some(Instant::now());
        }
        self.notify.notify_one();
    }

    pub fn buffer_snapshot(&self) -> Vec<SyncOp> {
        self.inner.lock().buffer.clone()
    }

    /// Checagem barata (sem clonar conteúdos) de fila não-vazia. Usada como
    /// guarda no caminho quente do FUSE (`refresh_if_needed`).
    pub fn has_pending_ops(&self) -> bool {
        !self.inner.lock().buffer.is_empty()
    }

    fn spawn_watcher(&self) {
        let debounce_secs = self.debounce_secs;
        let notify = self.notify.clone();
        let inner = self.inner.clone();
        let repo_path = self.repo_path.clone();
        let branch = self.branch.clone();
        let user_name = self.user_name.clone();
        let user_email = self.user_email.clone();
        let remote_name = self.remote_name.clone();
        let remote_token = self.remote_token.clone();
        let refresh_flag = self.refresh_flag.clone();
        let commit_generation = self.commit_generation.clone();
        let repo_lock = self.repo_lock.clone();
        let status = self.status.clone();

        task::spawn(async move {
            loop {
                notify.notified().await;

                loop {
                    sleep(Duration::from_secs(debounce_secs)).await;

                    let drained = {
                        let mut inner = inner.lock();
                        let should_drain = inner
                            .last_op
                            .map(|l| l.elapsed() >= Duration::from_secs(debounce_secs))
                            .unwrap_or(false)
                            && !inner.buffer.is_empty();
                        if !should_drain {
                            None
                        } else {
                            Some(std::mem::take(&mut inner.buffer))
                        }
                    };

                    if let Some(changes) = drained {
                        println!("[{}] drain: sincronizando alterações", log_ts());
                        // Serializa o ciclo inteiro (apply + commit + push +
                        // reconcile) com o pull watcher (RF4). Sem .await no
                        // escopo do guard.
                        let _repo_guard = repo_lock.lock();
                        // Aplicação na ordem de chegada — a fila já é causal.
                        let ops: Vec<&SyncOp> = changes.iter().collect();
                        for op in &ops {
                            apply_op_to_disk(&repo_path, op);
                        }
                        let commit_oid = commit_batch(
                            &repo_path,
                            &branch,
                            &user_name,
                            &user_email,
                            &ops,
                        );
                        let mut pushed: Option<bool> = None;
                        if let Some(_oid) = commit_oid {
                            commit_generation.fetch_add(1, Ordering::SeqCst);
                            if let Some(ref remote) = remote_name {
                                let ok = push_batch(&repo_path, &branch, remote, &remote_token);
                                if ok {
                                    pushed = Some(true);
                                } else {
                                    let recovered = reconcile_and_retry_push(
                                        &repo_path,
                                        &branch,
                                        remote,
                                        &remote_token,
                                        &user_name,
                                        &user_email,
                                        &status,
                                    );
                                    pushed = Some(recovered);
                                    if recovered {
                                        refresh_flag.store(true, Ordering::SeqCst);
                                    }
                                }
                            }
                        }
                        let pending = inner.lock().buffer.len();
                        status.record_drain(
                            ops.len(),
                            commit_oid.map(|o| o.to_string()),
                            pushed,
                            pending,
                        );
                        break;
                    }
                }
            }
        });
    }
}

fn apply_op_to_disk(repo_path: &Path, op: &SyncOp) {
    match op {
        SyncOp::Write { path, content } => {
            let full = repo_path.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            match std::fs::write(&full, content) {
                Ok(_) => println!("[{}] disk: write {}", log_ts(), path),
                Err(e) => eprintln!("[{}] disk: erro ao salvar {}: {}", log_ts(), path, e),
            }
        }
        SyncOp::Delete { path } => {
            let full = repo_path.join(path);
            match std::fs::remove_file(&full) {
                Ok(_) => println!("[{}] disk: unlink {}", log_ts(), path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Path nunca chegou ao disco (ex.: temp dirs `.sb-*` que o
                    // filtro bloqueia na criação mas deixa passar no delete).
                    // Idempotente: já não existe = remoção bem-sucedida.
                }
                Err(e) => eprintln!("[{}] disk: erro ao remover {}: {}", log_ts(), path, e),
            }
        }
        SyncOp::Mkdir { path } => {
            let full = repo_path.join(path);
            if let Err(e) = std::fs::create_dir_all(&full) {
                eprintln!("[{}] disk: erro ao criar diretório {}: {}", log_ts(), path, e);
                return;
            }
            let keep = full.join(".gitkeep");
            if !keep.exists() {
                if let Err(e) = std::fs::write(&keep, b"") {
                    eprintln!(
                        "[{}] disk: erro ao criar .gitkeep em {}: {}",
                        log_ts(),
                        path,
                        e
                    );
                    return;
                }
            }
            println!("[{}] disk: mkdir {}", log_ts(), path);
        }
        SyncOp::Rmdir { path } => {
            let full = repo_path.join(path);
            let keep = full.join(".gitkeep");
            let _ = std::fs::remove_file(&keep);
            match std::fs::remove_dir(&full) {
                Ok(_) => println!("[{}] disk: rmdir {}", log_ts(), path),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Mesmo racional do Delete: diretório que nunca chegou ao
                    // disco (ex.: `.sb-*`) gera ENOENT no rmdir. Silencia.
                }
                Err(e) => eprintln!(
                    "[{}] disk: erro ao remover diretório {}: {}",
                    log_ts(),
                    path,
                    e
                ),
            }
        }
        SyncOp::Rename { from, to } => {
            let from_full = repo_path.join(from);
            let to_full = repo_path.join(to);
            if let Some(parent) = to_full.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            match std::fs::rename(&from_full, &to_full) {
                Ok(_) => println!("[{}] disk: rename {} -> {}", log_ts(), from, to),
                Err(e) => eprintln!(
                    "[{}] disk: erro ao renomear {} -> {}: {}",
                    log_ts(),
                    from,
                    to,
                    e
                ),
            }
        }
    }
}

/// Retorna o oid do commit criado, ou `None` (sem identidade, tree limpa ou
/// erro — o motivo vai para o log).
fn commit_batch(
    repo_path: &Path,
    branch: &str,
    user_name: &Option<String>,
    user_email: &Option<String>,
    ops: &[&SyncOp],
) -> Option<git2::Oid> {
    let (Some(name), Some(email)) = (user_name.as_deref(), user_email.as_deref()) else {
        println!(
            "[{}] commit: user_name/user_email ausentes — commit ignorado",
            log_ts()
        );
        return None;
    };
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] commit: erro ao abrir repo: {}", log_ts(), e);
            return None;
        }
    };
    let message = build_commit_message(ops);
    match git::commit_working_tree(&repo, branch, name, email, &message) {
        Ok(Some(oid)) => {
            println!("[{}] commit: criado {}", log_ts(), oid);
            Some(oid)
        }
        Ok(None) => {
            println!("[{}] commit: tree limpa, nada a commitar", log_ts());
            None
        }
        Err(e) => {
            eprintln!("[{}] commit: erro: {}", log_ts(), e);
            None
        }
    }
}

fn push_batch(repo_path: &Path, branch: &str, remote: &str, token: &Option<String>) -> bool {
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] push: erro ao abrir repo: {}", log_ts(), e);
            return false;
        }
    };
    match git::push_to_remote(&repo, branch, remote, token.as_deref()) {
        Ok(_) => {
            println!("[{}] push: {} ok", log_ts(), remote);
            true
        }
        Err(e) => {
            eprintln!("[{}] push: erro em {}: {}", log_ts(), remote, e);
            false
        }
    }
}

/// Push falhou (geralmente non-FF). Faz fetch + pull_and_merge + retry de push único.
/// Retorna `true` se o retry teve sucesso (implica que o working tree mudou e o caller
/// deve sinalizar refresh pro FUSE).
fn reconcile_and_retry_push(
    repo_path: &Path,
    branch: &str,
    remote: &str,
    token: &Option<String>,
    user_name: &Option<String>,
    user_email: &Option<String>,
    status: &StatusState,
) -> bool {
    let (Some(name), Some(email)) = (user_name.as_deref(), user_email.as_deref()) else {
        eprintln!(
            "[{}] reconcile: push falhou e sem identidade — impossível reconciliar",
            log_ts()
        );
        return false;
    };
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] reconcile: erro ao abrir repo: {}", log_ts(), e);
            return false;
        }
    };

    if let Err(e) = git::fetch_remote(&repo, branch, remote, token.as_deref()) {
        eprintln!("[{}] reconcile: erro no fetch: {}", log_ts(), e);
        return false;
    }

    let suffix = conflict_suffix();
    match git::pull_and_merge(&repo, branch, remote, name, email, &suffix) {
        Ok(git::MergeOutcome::NoOp) => {
            eprintln!(
                "[{}] reconcile: sem divergência — erro original pode ser rede/auth",
                log_ts()
            );
            return false;
        }
        Ok(git::MergeOutcome::Conflicted { ref resolved, ref suffix }) => {
            println!(
                "[{}] reconcile: {} conflito(s) — locais em *.{}: {:?}",
                log_ts(),
                resolved.len(),
                suffix,
                resolved
            );
            status.add_conflicts(
                resolved
                    .iter()
                    .map(|p| format!("{}.{}", p, suffix))
                    .collect(),
            );
        }
        Ok(o) => {
            println!("[{}] reconcile: aplicado ({:?})", log_ts(), o);
        }
        Err(e) => {
            eprintln!("[{}] reconcile: erro no pull_and_merge: {}", log_ts(), e);
            return false;
        }
    }

    match git::push_to_remote(&repo, branch, remote, token.as_deref()) {
        Ok(_) => {
            println!("[{}] push: retry em {} ok", log_ts(), remote);
            true
        }
        Err(e) => {
            eprintln!("[{}] push: retry ainda falhou: {}", log_ts(), e);
            false
        }
    }
}

/// Gera sufixo único para conflicted copies. Formato: `conflict-<hostname>-<unix_ts>`.
fn conflict_suffix() -> String {
    let host = gethostname_or("unknown");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("conflict-{}-{}", host, ts)
}

fn gethostname_or(fallback: &str) -> String {
    let mut buf = [0u8; 256];
    let ret = unsafe {
        libc::gethostname(
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len() as libc::size_t,
        )
    };
    if ret != 0 {
        return fallback.to_string();
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => fallback.to_string(),
    }
}

fn build_commit_message(ops: &[&SyncOp]) -> String {
    let n = ops.len();
    let paths: Vec<&str> = ops.iter().map(|op| op.primary_path()).collect();
    let preview: Vec<&str> = paths.iter().take(3).copied().collect();
    let more = if n > preview.len() {
        format!(", +{} more", n - preview.len())
    } else {
        String::new()
    };
    format!("drive-sync: {} changes ({}{})", n, preview.join(", "), more)
}

/// Paths que o macOS gera como ruído e que nunca devem vazar pro working tree/commit.
/// Detecta qualquer componente do caminho que bata com um padrão conhecido.
/// Paths que nunca devem aparecer no working tree — sujeira gerada e deletada
/// pelo próprio macOS (AppleDouble, metadados de Finder/Spotlight/Trash).
/// Filtramos TODAS as ops: a gente não quer nem o rastro delas em commit.
pub(crate) fn is_fully_ephemeral_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    for component in path.split('/') {
        if component.starts_with("._") {
            return true;
        }
        if matches!(
            component,
            ".DS_Store"
                | ".Spotlight-V100"
                | ".Trashes"
                | ".TemporaryItems"
                | ".fseventsd"
                | ".DocumentRevisions-V100"
                | ".apdisk"
                | ".VolumeIcon.icns"
        ) {
            return true;
        }
    }
    false
}

/// Padrão do safe-save do TextEdit/NSDocument: `<nome>.sb-<hex>-<rand>`.
/// Temp dirs e backups hardlinkados. Usamos esse padrão só pra bloquear a
/// **criação** (Write/Mkdir). Delete/Rmdir/Rename passam, pra que o usuário
/// possa limpar resíduos `.sb-*` que já estejam commitados no repo.
fn is_textedit_scratch_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    path.split('/').any(|c| c.contains(".sb-"))
}

fn op_touches_ephemeral(op: &SyncOp) -> bool {
    match op {
        SyncOp::Write { path, .. } | SyncOp::Mkdir { path } => {
            is_fully_ephemeral_path(path) || is_textedit_scratch_path(path)
        }
        SyncOp::Delete { path } | SyncOp::Rmdir { path } => is_fully_ephemeral_path(path),
        SyncOp::Rename { from, to } => {
            is_fully_ephemeral_path(from)
                || is_fully_ephemeral_path(to)
                || is_textedit_scratch_path(to)
        }
    }
}

/// `true` se `a` e `b` são o mesmo path ou um é ancestral do outro
/// (limite de componente em '/': "a" contém "a/b", mas não "ab").
fn paths_related(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_contains_b = b
        .strip_prefix(a)
        .map(|rest| rest.starts_with('/'))
        .unwrap_or(false);
    let b_contains_a = a
        .strip_prefix(b)
        .map(|rest| rest.starts_with('/'))
        .unwrap_or(false);
    a_contains_b || b_contains_a
}

/// Paths que uma op toca (rename toca origem e destino).
fn op_paths(op: &SyncOp) -> [Option<&str>; 2] {
    match op {
        SyncOp::Rename { from, to } => [Some(from), Some(to)],
        other => [Some(other.primary_path()), None],
    }
}

/// Anexa `op` à fila preservando a ordem de chegada. Única coalescência
/// permitida: `Write` substitui in-place o `Write` mais recente do MESMO path,
/// desde que nenhuma op posterior a ele toque um path relacionado (igual ou
/// ancestral/descendente) — rename, delete, mkdir e rmdir são barreiras.
/// A correção nunca depende da coalescência; ela só evita conteúdo redundante
/// em write storms (flushes repetidos do mesmo arquivo dentro do debounce).
pub(crate) fn push_coalescing(queue: &mut Vec<SyncOp>, op: SyncOp) {
    if let SyncOp::Write { path, content } = op {
        for existing in queue.iter_mut().rev() {
            if let SyncOp::Write { path: p, content: c } = existing {
                if *p == path {
                    *c = content;
                    return;
                }
            }
            let blocking = op_paths(existing)
                .into_iter()
                .flatten()
                .any(|p| paths_related(p, &path));
            if blocking {
                break;
            }
        }
        queue.push(SyncOp::Write { path, content });
    } else {
        queue.push(op);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr() -> SyncManager {
        SyncManager::new("/tmp/ignored", 60)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_appends_to_buffer() {
        let m = mgr();
        m.queue_write("a.txt", b"hello".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { path, content } => {
                assert_eq!(path, "a.txt");
                assert_eq!(content.as_slice(), b"hello");
            }
            other => panic!("esperado Write, obtido {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_twice_same_path_coalesces() {
        let m = mgr();
        m.queue_write("a.txt", b"first".to_vec());
        m.queue_write("a.txt", b"second".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { content, .. } => assert_eq!(content.as_slice(), b"second"),
            other => panic!("esperado Write, obtido {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_mkdir_then_rmdir_same_path_keeps_causal_order() {
        // Semântica da fila causal (Fase 0): as duas ops são mantidas na ordem
        // de chegada — o drain aplica mkdir e depois rmdir (estado final
        // idêntico, sem depender de heurística de "última ganha").
        let m = mgr();
        m.queue_mkdir("foo");
        m.queue_rmdir("foo");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 2);
        assert!(matches!(snap[0], SyncOp::Mkdir { .. }));
        assert!(matches!(snap[1], SyncOp::Rmdir { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_rename_then_write_to_destination_keeps_both_in_order() {
        // Antes (HashMap keyed por destino), o Write APAGAVA o Rename da fila.
        // Na fila causal ambos sobrevivem, na ordem de chegada.
        let m = mgr();
        m.queue_rename("a.txt", "b.txt");
        m.queue_write("b.txt", b"x".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 2);
        assert!(matches!(snap[0], SyncOp::Rename { .. }));
        assert!(matches!(snap[1], SyncOp::Write { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_delete_creates_delete_op() {
        let m = mgr();
        m.queue_delete("x.txt");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Delete { path } => assert_eq!(path, "x.txt"),
            other => panic!("esperado Delete, obtido {:?}", other),
        }
    }

    #[test]
    fn is_ephemeral_path_flags_apple_double_and_system_dirs() {
        assert!(is_fully_ephemeral_path("._hi.txt"));
        assert!(is_fully_ephemeral_path("foo/._hi.txt"));
        assert!(is_fully_ephemeral_path(".DS_Store"));
        assert!(is_fully_ephemeral_path("docs/.DS_Store"));
        assert!(is_fully_ephemeral_path(".Spotlight-V100"));
        assert!(is_fully_ephemeral_path(".Trashes/whatever"));
        assert!(!is_fully_ephemeral_path("hi.txt"));
        assert!(!is_fully_ephemeral_path("foo/hi.txt"));
        // .sb-* não entra em `is_ephemeral_path` (filtro total) — só em
        // `is_textedit_scratch_path`, que é parcial.
        assert!(!is_fully_ephemeral_path("hi.txt.sb-36d9af71-iFhNoR"));
        assert!(!is_fully_ephemeral_path(""));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn textedit_scratch_paths_block_creation_but_allow_delete() {
        let m = mgr();
        // Criação: filtra
        m.queue_write("hi.txt.sb-36d9af71-backup", b"x".to_vec());
        m.queue_mkdir("hi.txt.sb-36d9af71-tempdir");
        // Delete/rmdir: passa (usuário consegue limpar `.sb-*` legado)
        m.queue_delete("hi.txt.sb-36d9af71-old-backup");
        m.queue_rmdir("hi.txt.sb-36d9af71-old-tempdir");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 2, "esperado só Delete+Rmdir, obtido {:?}", snap);
        assert!(snap.iter().any(|op| matches!(op, SyncOp::Delete { .. })));
        assert!(snap.iter().any(|op| matches!(op, SyncOp::Rmdir { .. })));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_drops_ephemeral_path() {
        let m = mgr();
        m.queue_write("._hi.txt", b"junk".to_vec());
        m.queue_write(".DS_Store", b"junk".to_vec());
        m.queue_write("hi.txt", b"real".to_vec());
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { path, .. } => assert_eq!(path, "hi.txt"),
            other => panic!("esperado Write para hi.txt, obtido {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_rename_drops_when_either_side_is_ephemeral() {
        let m = mgr();
        m.queue_rename("hi.txt", "._hi.txt");
        m.queue_rename("._hi.txt", "hi.txt");
        assert_eq!(m.buffer_snapshot().len(), 0);
    }

    #[test]
    fn push_coalescing_write_over_write_same_path_replaces_in_place() {
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "a.txt".into(), content: b"v1".to_vec() });
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: b"other".to_vec() });
        push_coalescing(&mut q, SyncOp::Write { path: "a.txt".into(), content: b"v2".to_vec() });
        assert_eq!(q.len(), 2);
        assert!(matches!(&q[0], SyncOp::Write { path, content }
            if path == "a.txt" && content.as_slice() == b"v2"));
    }

    #[test]
    fn push_coalescing_rename_is_barrier_for_write() {
        // Write(b) não pode atravessar Rename{a->b}: o conteúdo pré e pós
        // rename são estados diferentes do path.
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: b"pre".to_vec() });
        push_coalescing(&mut q, SyncOp::Rename { from: "a.txt".into(), to: "b.txt".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: b"post".to_vec() });
        assert_eq!(q.len(), 3);
        assert!(matches!(&q[2], SyncOp::Write { content, .. } if content.as_slice() == b"post"));
    }

    #[test]
    fn push_coalescing_dir_rename_is_barrier_for_nested_write() {
        // Rename de diretório é barreira para writes DENTRO dele (prefixo).
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "d/f.txt".into(), content: b"v1".to_vec() });
        push_coalescing(&mut q, SyncOp::Rename { from: "c".into(), to: "d".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "d/f.txt".into(), content: b"v2".to_vec() });
        assert_eq!(q.len(), 3, "write pós-rename não pode coalescer com o pré-rename");
    }

    #[test]
    fn paths_related_respects_component_boundary() {
        assert!(paths_related("a", "a"));
        assert!(paths_related("a", "a/b"));
        assert!(paths_related("a/b/c", "a/b"));
        assert!(!paths_related("a", "ab"));
        assert!(!paths_related("ab", "a"));
        assert!(!paths_related("a/b", "a/bc"));
    }

    #[test]
    fn build_commit_message_three_paths() {
        let ops_owned = [
            SyncOp::Write { path: "a.txt".into(), content: vec![] },
            SyncOp::Write { path: "b.txt".into(), content: vec![] },
            SyncOp::Write { path: "c.txt".into(), content: vec![] },
        ];
        let ops: Vec<&SyncOp> = ops_owned.iter().collect();
        let m = build_commit_message(&ops);
        assert!(m.contains("3 changes"), "{}", m);
        assert!(m.contains("a.txt"));
    }

    #[test]
    fn build_commit_message_more_than_three_truncates() {
        let ops_owned: Vec<SyncOp> = (0..5)
            .map(|i| SyncOp::Write {
                path: format!("f{}.txt", i),
                content: vec![],
            })
            .collect();
        let ops: Vec<&SyncOp> = ops_owned.iter().collect();
        let m = build_commit_message(&ops);
        assert!(m.contains("5 changes"), "{}", m);
        assert!(m.contains("+2 more"), "{}", m);
    }
}
