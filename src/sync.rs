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
use crate::notify::{self, Notifier, NotifyEvent};
use crate::status::StatusState;

/// Conteúdo de um `SyncOp::Write`. `Mem` compartilha o mesmo `Arc` do write
/// buffer do inode (pico de RAM ~1x o tamanho do arquivo, não uma cópia por
/// estágio da pipeline).
#[derive(Debug, Clone)]
pub enum WriteContent {
    Mem(Arc<Vec<u8>>),
    /// Snapshot em arquivo de spool (dentro de `.git/`, mesmo volume do
    /// working tree): o drain aplica com `rename`, sem materializar em RAM.
    Spooled(PathBuf),
}

impl WriteContent {
    /// Materializa o conteúdo (cópia). Uso: testes e caminhos frios.
    pub fn to_vec(&self) -> Vec<u8> {
        match self {
            WriteContent::Mem(a) => a.as_ref().clone(),
            WriteContent::Spooled(p) => std::fs::read(p).unwrap_or_default(),
        }
    }

    pub fn len(&self) -> u64 {
        match self {
            WriteContent::Mem(a) => a.len() as u64,
            WriteContent::Spooled(p) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone)]
pub enum SyncOp {
    Write { path: String, content: WriteContent },
    Delete { path: String },
    Mkdir { path: String },
    Rmdir { path: String },
    Rename { from: String, to: String },
    /// Criação de symlink com alvo opaco (texto). Barreira na fila, como as
    /// demais ops não-Write — nunca coalesce.
    Symlink { path: String, target: String },
}

impl SyncOp {
    pub fn primary_path(&self) -> &str {
        match self {
            SyncOp::Write { path, .. } => path,
            SyncOp::Delete { path } => path,
            SyncOp::Mkdir { path } => path,
            SyncOp::Rmdir { path } => path,
            SyncOp::Rename { to, .. } => to,
            SyncOp::Symlink { path, .. } => path,
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
    /// Notificações macOS (nasce desligado; o modo mount real habilita).
    notifier: Arc<Notifier>,
    /// Limiar em bytes para conversão em manifest + chunks no commit
    /// (0 = chunking desligado). Setter pós-construção, como o status file.
    chunk_threshold_bytes: Arc<AtomicU64>,
    /// Tamanho de cada chunk, em bytes.
    chunk_size_bytes: Arc<AtomicU64>,
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
            notifier: Arc::new(Notifier::new()),
            chunk_threshold_bytes: Arc::new(AtomicU64::new(95 * 1024 * 1024)),
            chunk_size_bytes: Arc::new(AtomicU64::new(80 * 1024 * 1024)),
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

    /// Configura o chunking de arquivos grandes (Fase 2.6). `threshold_bytes`
    /// 0 desativa. Setter pós-construção para não estender a assinatura já
    /// extensa de `new_full_with_pull`.
    pub fn set_chunking(&self, threshold_bytes: u64, chunk_size_bytes: u64) {
        self.chunk_threshold_bytes
            .store(threshold_bytes, Ordering::SeqCst);
        self.chunk_size_bytes
            .store(chunk_size_bytes.max(1), Ordering::SeqCst);
    }

    /// Snapshot dos campos compartilhados do ciclo de drain.
    fn drain_ctx(&self) -> DrainCtx {
        DrainCtx {
            inner: self.inner.clone(),
            repo_path: self.repo_path.clone(),
            branch: self.branch.clone(),
            user_name: self.user_name.clone(),
            user_email: self.user_email.clone(),
            remote_name: self.remote_name.clone(),
            remote_token: self.remote_token.clone(),
            refresh_flag: self.refresh_flag.clone(),
            commit_generation: self.commit_generation.clone(),
            repo_lock: self.repo_lock.clone(),
            status: self.status.clone(),
            notifier: self.notifier.clone(),
            chunk_threshold_bytes: self.chunk_threshold_bytes.clone(),
            chunk_size_bytes: self.chunk_size_bytes.clone(),
        }
    }

    /// Drena a fila AGORA, de forma síncrona (aplica + commita + push, sob o
    /// `repo_lock`). Usado no shutdown limpo: depois do unmount, garante que
    /// nenhuma op enfileirada se perca com a morte do processo.
    pub fn drain_now(&self) {
        let changes = { std::mem::take(&mut self.inner.lock().buffer) };
        if changes.is_empty() {
            return;
        }
        println!(
            "[{}] drain_now: drenando {} op(s) pendentes no shutdown",
            log_ts(),
            changes.len()
        );
        self.drain_ctx().run(changes);
    }

    /// Acesso ao estado observável (snapshot para testes/CLI embutido).
    pub fn status_state(&self) -> Arc<StatusState> {
        self.status.clone()
    }

    /// Acesso ao notificador (o main usa para eventos de mount/parada).
    pub fn notifier(&self) -> Arc<Notifier> {
        self.notifier.clone()
    }

    /// Habilita as notificações do macOS (modo mount real).
    pub fn enable_notifications(&self) {
        self.notifier.enable();
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
        let notifier = self.notifier.clone();

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
                    Ok(true) => {
                        // Condição de bloqueio resolvida — rearma a notificação.
                        notifier.reset(notify::KEY_PULL_BLOCKED);
                    }
                    Ok(false) => {
                        println!(
                            "[{}] pull: working tree com mudanças não commitadas — pulando",
                            log_ts()
                        );
                        status.record_pull("pulado: working tree sujo");
                        notifier.notify(NotifyEvent::PullBlocked);
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
                        let copies: Vec<String> = resolved
                            .iter()
                            .map(|p| format!("{}.{}", p, suffix))
                            .collect();
                        status.add_conflicts(copies.clone());
                        notifier.notify(NotifyEvent::Conflicts { copies });
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
        self.queue_write_content(path, WriteContent::Mem(Arc::new(content)));
    }

    /// Enfileira um write com conteúdo já empacotado (ex.: `Arc` compartilhado
    /// com o write buffer do FUSE — nenhuma cópia no caminho quente).
    pub fn queue_write_content(&self, path: &str, content: WriteContent) {
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

    pub fn queue_symlink(&self, path: &str, target: &str) {
        self.enqueue(SyncOp::Symlink {
            path: path.to_string(),
            target: target.to_string(),
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
        let ctx = Arc::new(self.drain_ctx());

        task::spawn(async move {
            loop {
                notify.notified().await;

                loop {
                    sleep(Duration::from_secs(debounce_secs)).await;

                    let drained = {
                        let mut inner = ctx.inner.lock();
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
                        ctx.run(changes);
                        break;
                    }
                }
            }
        });
    }
}

/// Contexto compartilhado do ciclo de drain — o watcher de debounce e o
/// `drain_now` do shutdown executam exatamente o mesmo corpo.
struct DrainCtx {
    inner: Arc<Mutex<Inner>>,
    repo_path: PathBuf,
    branch: String,
    user_name: Option<String>,
    user_email: Option<String>,
    remote_name: Option<String>,
    remote_token: Option<String>,
    refresh_flag: Arc<AtomicBool>,
    commit_generation: Arc<AtomicU64>,
    repo_lock: Arc<Mutex<()>>,
    status: Arc<StatusState>,
    notifier: Arc<Notifier>,
    chunk_threshold_bytes: Arc<AtomicU64>,
    chunk_size_bytes: Arc<AtomicU64>,
}

impl DrainCtx {
    fn run(&self, changes: Vec<SyncOp>) {
        // Serializa o ciclo inteiro (apply + commit + push + reconcile) com o
        // pull watcher (RF4 da Fase 0). Sem .await no escopo do guard.
        let _repo_guard = self.repo_lock.lock();
        // Aplicação na ordem de chegada — a fila já é causal.
        let ops: Vec<&SyncOp> = changes.iter().collect();
        for op in &ops {
            apply_op_to_disk(&self.repo_path, op);
        }
        // Chunking (Fase 2.6): a varredura converte QUALQUER arquivo bruto
        // acima do limiar — dos ops recém-aplicados aos que chegaram por fora
        // do FUSE. Invariante: nenhum blob acima do limiar é stageado. O GC
        // remove chunks que nenhum manifest do working tree referencia mais.
        let chunk_threshold = self.chunk_threshold_bytes.load(Ordering::SeqCst);
        let mut converted = 0usize;
        if chunk_threshold > 0 {
            let chunk_size = self.chunk_size_bytes.load(Ordering::SeqCst).max(1);
            converted =
                convert_oversized_worktree_files(&self.repo_path, chunk_threshold, chunk_size);
            gc_orphan_chunks(&self.repo_path);
        }
        let commit_oid = commit_batch(
            &self.repo_path,
            &self.branch,
            &self.user_name,
            &self.user_email,
            &ops,
        );
        let mut pushed: Option<bool> = None;
        if let Some(_oid) = commit_oid {
            self.commit_generation.fetch_add(1, Ordering::SeqCst);
            // Conversão da varredura cria paths novos no HEAD que o índice do
            // FUSE pode não conhecer (arquivo chegou por fora do mount) — sem
            // o refresh, o arquivo ficaria invisível no drive até um pull real
            // (incidente da migração do zip, 03-07-2026). Flag DEPOIS do
            // commit: rebuild antes dele leria o HEAD antigo e consumiria a
            // flag à toa.
            if converted > 0 {
                self.refresh_flag.store(true, Ordering::SeqCst);
            }
            if let Some(ref remote) = self.remote_name {
                let ok = push_batch(&self.repo_path, &self.branch, remote, &self.remote_token);
                if ok {
                    pushed = Some(true);
                } else {
                    let recovered = reconcile_and_retry_push(
                        &self.repo_path,
                        &self.branch,
                        remote,
                        &self.remote_token,
                        &self.user_name,
                        &self.user_email,
                        &self.status,
                        &self.notifier,
                    );
                    pushed = Some(recovered);
                    if recovered {
                        self.refresh_flag.store(true, Ordering::SeqCst);
                    } else {
                        // Push + reconcile falharam: commit local sem publicar.
                        self.notifier.notify(NotifyEvent::PushFailed);
                    }
                }
            }
        }
        let pending = self.inner.lock().buffer.len();
        self.status.record_drain(
            ops.len(),
            commit_oid.map(|o| o.to_string()),
            pushed,
            pending,
        );
    }
}

fn apply_op_to_disk(repo_path: &Path, op: &SyncOp) {
    match op {
        SyncOp::Write { path, content } => {
            let full = repo_path.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            let result = match content {
                WriteContent::Mem(data) => std::fs::write(&full, data.as_slice()),
                // rename move o snapshot sem passar pela RAM; fallback para
                // copy+remove se o spool estiver em outro volume.
                WriteContent::Spooled(src) => std::fs::rename(src, &full).or_else(|_| {
                    std::fs::copy(src, &full).map(|_| ())?;
                    let _ = std::fs::remove_file(src);
                    Ok(())
                }),
            };
            match result {
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
        SyncOp::Symlink { path, target } => {
            let full = repo_path.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            // symlink(2) falha com EEXIST se o path existe — remover antes
            // reproduz a semântica da fila (o estado final é o symlink).
            let _ = std::fs::remove_file(&full);
            match std::os::unix::fs::symlink(target, &full) {
                Ok(_) => println!("[{}] disk: symlink {} -> {}", log_ts(), path, target),
                Err(e) => eprintln!(
                    "[{}] disk: erro ao criar symlink {} -> {}: {}",
                    log_ts(),
                    path,
                    target,
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
#[allow(clippy::too_many_arguments)]
fn reconcile_and_retry_push(
    repo_path: &Path,
    branch: &str,
    remote: &str,
    token: &Option<String>,
    user_name: &Option<String>,
    user_email: &Option<String>,
    status: &StatusState,
    notifier: &Notifier,
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
            let copies: Vec<String> = resolved
                .iter()
                .map(|p| format!("{}.{}", p, suffix))
                .collect();
            status.add_conflicts(copies.clone());
            notifier.notify(NotifyEvent::Conflicts { copies });
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

/// Varre o working tree e converte em manifest + chunks todo arquivo regular
/// acima de `threshold` que não seja ignorado nem já manifest. Pula `.git`
/// (em qualquer nível) e `.chunks`. Streaming: RAM limitada a um chunk.
fn convert_oversized_worktree_files(repo_path: &Path, threshold: u64, chunk_size: u64) -> usize {
    let repo = match git::open_repo(repo_path.to_str().unwrap_or("")) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[{}] chunk: erro ao abrir repo: {}", log_ts(), e);
            return 0;
        }
    };
    let mut converted = 0usize;
    for (full, rel) in walk_worktree_files(repo_path) {
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        if !meta.is_file() || meta.len() <= threshold {
            continue;
        }
        // Ignorados são local-only: nunca commitam, chunkar quebraria a leitura.
        if repo.is_path_ignored(&rel).unwrap_or(false) {
            continue;
        }
        if file_starts_with_manifest_magic(&full) {
            continue;
        }
        match convert_file_to_chunks(repo_path, &full, chunk_size) {
            Ok(manifest) => {
                converted += 1;
                println!(
                    "[{}] chunk: {} -> {} chunk(s) de até {} bytes",
                    log_ts(),
                    rel,
                    manifest.hashes.len(),
                    chunk_size
                );
            }
            Err(e) => eprintln!("[{}] chunk: erro ao converter {}: {}", log_ts(), rel, e),
        }
    }
    converted
}

/// Converte um arquivo bruto em chunks + manifest (substitui o arquivo pelo
/// manifest via temp + rename).
fn convert_file_to_chunks(
    repo_root: &Path,
    full: &Path,
    chunk_size: u64,
) -> std::io::Result<crate::chunk::Manifest> {
    let mut reader = std::fs::File::open(full)?;
    let manifest = crate::chunk::chunk_reader_into_repo(&mut reader, chunk_size, repo_root)?;
    let tmp = full.with_extension("drive-sync-manifest.tmp");
    std::fs::write(&tmp, manifest.serialize())?;
    std::fs::rename(&tmp, full)?;
    Ok(manifest)
}

/// Remove de `.chunks/` os arquivos que nenhum manifest do working tree
/// referencia. Barato quando não há chunks (early return).
fn gc_orphan_chunks(repo_path: &Path) {
    let chunks_root = repo_path.join(crate::chunk::CHUNKS_DIR);
    let existing: Vec<std::path::PathBuf> = walk_dir_files(&chunks_root);
    if existing.is_empty() {
        return;
    }
    let mut referenced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (full, _) in walk_worktree_files(repo_path) {
        let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
        if !meta.is_file() || meta.len() > crate::chunk::MANIFEST_MAX_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&full) else { continue };
        if crate::chunk::Manifest::is_manifest_bytes(&bytes) {
            if let Ok(m) = crate::chunk::Manifest::parse(&bytes) {
                referenced.extend(m.hashes);
            }
        }
    }
    for chunk_file in existing {
        let name = chunk_file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !referenced.contains(&name) {
            match std::fs::remove_file(&chunk_file) {
                Ok(_) => println!("[{}] chunk: gc removeu órfão {}", log_ts(), name),
                Err(e) => eprintln!("[{}] chunk: gc falhou em {}: {}", log_ts(), name, e),
            }
            // Shard vazio some junto (best-effort).
            if let Some(parent) = chunk_file.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

/// Arquivos do working tree como (path completo, path relativo), pulando
/// `.git` em qualquer nível e `.chunks` na raiz. Não segue symlinks.
fn walk_worktree_files(repo_path: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = vec![repo_path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let full = entry.path();
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            if dir == repo_path && name == crate::chunk::CHUNKS_DIR {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&full) else { continue };
            if meta.is_dir() {
                stack.push(full);
            } else if meta.is_file() {
                let rel = full
                    .strip_prefix(repo_path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push((full, rel));
            }
        }
    }
    out
}

/// Arquivos (recursivo) sob um diretório. Vazio se o diretório não existe.
fn walk_dir_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let Ok(meta) = std::fs::symlink_metadata(&p) else { continue };
            if meta.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// `true` se o arquivo começa com a magic line de manifest (leitura de poucos
/// bytes, sem carregar o arquivo).
fn file_starts_with_manifest_magic(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 64];
    let Ok(n) = f.read(&mut buf) else { return false };
    crate::chunk::Manifest::is_manifest_bytes(&buf[..n])
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
        SyncOp::Write { path, .. } | SyncOp::Mkdir { path } | SyncOp::Symlink { path, .. } => {
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
                    // Snapshot spooled substituído nunca chegará ao drain —
                    // remover o arquivo aqui evita acúmulo no diretório de spool.
                    if let WriteContent::Spooled(old) = &*c {
                        let _ = std::fs::remove_file(old);
                    }
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

    /// Empacota bytes em WriteContent::Mem para os testes de fila.
    fn wc(bytes: &[u8]) -> WriteContent {
        WriteContent::Mem(Arc::new(bytes.to_vec()))
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
                assert_eq!(content.to_vec(), b"hello");
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
            SyncOp::Write { content, .. } => assert_eq!(content.to_vec(), b"second"),
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
        push_coalescing(&mut q, SyncOp::Write { path: "a.txt".into(), content: wc(b"v1") });
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: wc(b"other") });
        push_coalescing(&mut q, SyncOp::Write { path: "a.txt".into(), content: wc(b"v2") });
        assert_eq!(q.len(), 2);
        assert!(matches!(&q[0], SyncOp::Write { path, content }
            if path == "a.txt" && content.to_vec() == b"v2"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_symlink_appends_and_respects_ephemeral_filter() {
        let m = mgr();
        m.queue_symlink("alias.txt", "real.txt");
        m.queue_symlink(".DS_Store", "x");
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1, "path efêmero deve ser filtrado: {:?}", snap);
        match &snap[0] {
            SyncOp::Symlink { path, target } => {
                assert_eq!(path, "alias.txt");
                assert_eq!(target, "real.txt");
            }
            other => panic!("esperado Symlink, obtido {:?}", other),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn queue_write_content_shares_arc_without_copy() {
        // Requisito 16 da Fase 2.5: buffer e SyncOp::Write compartilham o
        // MESMO conteúdo (Arc) — pico de RAM ~1x, não ~3x o tamanho.
        let m = mgr();
        let content = std::sync::Arc::new(b"conteudo grande".to_vec());
        m.queue_write_content("a.txt", WriteContent::Mem(content.clone()));
        let snap = m.buffer_snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            SyncOp::Write { content: WriteContent::Mem(arc), .. } => {
                assert!(
                    std::sync::Arc::ptr_eq(arc, &content),
                    "conteúdo deve ser compartilhado por Arc, não clonado"
                );
            }
            other => panic!("esperado Write compartilhado, obtido {:?}", other),
        }
    }

    #[test]
    fn push_coalescing_symlink_is_barrier_for_write() {
        // Write pós-symlink não pode coalescer com Write pré-symlink no mesmo
        // path: `rm f && ln -s alvo f && echo x > f` são três estados causais.
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "f.txt".into(), content: wc(b"pre") });
        push_coalescing(&mut q, SyncOp::Symlink { path: "f.txt".into(), target: "alvo".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "f.txt".into(), content: wc(b"post") });
        assert_eq!(q.len(), 3, "Symlink deve ser barreira: {:?}", q);
        assert!(matches!(&q[2], SyncOp::Write { content, .. } if content.to_vec() == b"post"));
    }

    #[test]
    fn push_coalescing_rename_is_barrier_for_write() {
        // Write(b) não pode atravessar Rename{a->b}: o conteúdo pré e pós
        // rename são estados diferentes do path.
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: wc(b"pre") });
        push_coalescing(&mut q, SyncOp::Rename { from: "a.txt".into(), to: "b.txt".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "b.txt".into(), content: wc(b"post") });
        assert_eq!(q.len(), 3);
        assert!(matches!(&q[2], SyncOp::Write { content, .. } if content.to_vec() == b"post"));
    }

    #[test]
    fn push_coalescing_dir_rename_is_barrier_for_nested_write() {
        // Rename de diretório é barreira para writes DENTRO dele (prefixo).
        let mut q = Vec::new();
        push_coalescing(&mut q, SyncOp::Write { path: "d/f.txt".into(), content: wc(b"v1") });
        push_coalescing(&mut q, SyncOp::Rename { from: "c".into(), to: "d".into() });
        push_coalescing(&mut q, SyncOp::Write { path: "d/f.txt".into(), content: wc(b"v2") });
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
            SyncOp::Write { path: "a.txt".into(), content: wc(b"") },
            SyncOp::Write { path: "b.txt".into(), content: wc(b"") },
            SyncOp::Write { path: "c.txt".into(), content: wc(b"") },
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
                content: wc(b""),
            })
            .collect();
        let ops: Vec<&SyncOp> = ops_owned.iter().collect();
        let m = build_commit_message(&ops);
        assert!(m.contains("5 changes"), "{}", m);
        assert!(m.contains("+2 more"), "{}", m);
    }
}
