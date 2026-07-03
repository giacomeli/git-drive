//! Estado observável do daemon, gravado em `logs/status.json` e lido pelo
//! subcomando `drive-sync status`. Timestamps em segundos unix (u64) — a
//! formatação humana acontece só no CLI (decisão 10 da Fase 1).

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::log_ts;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SyncStatus {
    pub pid: u32,
    pub repo: String,
    pub branch: String,
    pub started_at: u64,
    pub updated_at: u64,
    pub pending_ops: usize,
    pub last_drain: Option<DrainInfo>,
    pub last_pull: Option<PullInfo>,
    /// Conflicted copies criadas nesta sessão (paths das cópias `*.conflict-*`).
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainInfo {
    pub at: u64,
    pub ops: usize,
    pub commit: Option<String>,
    /// `None` = sem remote configurado ou sem commit; `Some(false)` = push
    /// falhou mesmo após o reconcile.
    pub pushed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullInfo {
    pub at: u64,
    pub outcome: String,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Estado em memória + persistência opcional em arquivo. Sempre presente no
/// `SyncManager`; o arquivo só é escrito depois de `enable_file`.
pub struct StatusState {
    inner: Mutex<SyncStatus>,
    file: Mutex<Option<PathBuf>>,
}

impl Default for StatusState {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusState {
    pub fn new() -> Self {
        StatusState {
            inner: Mutex::new(SyncStatus::default()),
            file: Mutex::new(None),
        }
    }

    pub fn record_boot(&self, repo: &str, branch: &str) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.pid = std::process::id();
        s.repo = repo.to_string();
        s.branch = branch.to_string();
        s.started_at = now;
        s.updated_at = now;
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Liga a persistência e grava o estado corrente imediatamente.
    pub fn enable_file(&self, path: PathBuf) {
        *self.file.lock() = Some(path);
        let snapshot = self.inner.lock().clone();
        self.persist(&snapshot);
    }

    pub fn record_drain(
        &self,
        ops: usize,
        commit: Option<String>,
        pushed: Option<bool>,
        pending_ops: usize,
    ) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.updated_at = now;
        s.pending_ops = pending_ops;
        s.last_drain = Some(DrainInfo {
            at: now,
            ops,
            commit,
            pushed,
        });
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    pub fn record_pull(&self, outcome: &str) {
        let now = now_unix();
        let mut s = self.inner.lock();
        s.updated_at = now;
        s.last_pull = Some(PullInfo {
            at: now,
            outcome: outcome.to_string(),
        });
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    /// Registra conflicted copies criadas em merges (pull ou reconcile).
    pub fn add_conflicts(&self, copies: Vec<String>) {
        if copies.is_empty() {
            return;
        }
        let mut s = self.inner.lock();
        s.updated_at = now_unix();
        s.conflicts.extend(copies);
        let snapshot = s.clone();
        drop(s);
        self.persist(&snapshot);
    }

    pub fn snapshot(&self) -> SyncStatus {
        self.inner.lock().clone()
    }

    fn persist(&self, status: &SyncStatus) {
        let Some(path) = self.file.lock().clone() else {
            return;
        };
        if let Err(e) = write_atomic(&path, status) {
            eprintln!(
                "[{}] status: erro ao gravar {}: {}",
                log_ts(),
                path.display(),
                e
            );
        }
    }
}

/// Escrita atômica: serializa num arquivo temporário ao lado do destino e
/// renomeia por cima — leitores nunca veem JSON pela metade.
pub fn write_atomic(path: &Path, status: &SyncStatus) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_vec_pretty(status).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn read_from(path: &Path) -> Result<SyncStatus, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("não foi possível ler {}: {}", path.display(), e))?;
    serde_json::from_str(&content)
        .map_err(|e| format!("JSON inválido em {}: {}", path.display(), e))
}

/// `Some(true/false)` se foi possível checar o processo; `None` se pid inválido.
pub fn pid_alive(pid: u32) -> Option<bool> {
    if pid == 0 {
        return None;
    }
    Some(unsafe { libc::kill(pid as libc::pid_t, 0) } == 0)
}

fn format_ago(now: u64, then: u64) -> String {
    if then == 0 || then > now {
        return "instante desconhecido".to_string();
    }
    let secs = now - then;
    if secs < 60 {
        format!("há {} s", secs)
    } else if secs < 3600 {
        format!("há {} min", secs / 60)
    } else {
        format!("há {} h {} min", secs / 3600, (secs % 3600) / 60)
    }
}

/// Formata o status para o terminal (pt-BR). Pura: recebe `now` e o resultado
/// da checagem de pid para ser testável sem processo real.
pub fn format_status(s: &SyncStatus, now: u64, alive: Option<bool>) -> String {
    let mut out = String::new();
    out.push_str(&format!("drive-sync — branch {}\n", s.branch));
    out.push_str(&format!("  repo: {}\n", s.repo));
    let daemon_line = match alive {
        Some(true) => format!("rodando (pid {})", s.pid),
        Some(false) => format!("POSSIVELMENTE PARADO (pid {} não responde)", s.pid),
        None => "desconhecido".to_string(),
    };
    out.push_str(&format!("  daemon: {}\n", daemon_line));
    out.push_str(&format!(
        "  iniciado {} | atualizado {}\n",
        format_ago(now, s.started_at),
        format_ago(now, s.updated_at)
    ));
    if alive == Some(true) && now.saturating_sub(s.updated_at) > 600 {
        out.push_str("  atenção: sem atualização há mais de 10 min — verifique os logs\n");
    }
    out.push_str(&format!("  ops pendentes: {}\n", s.pending_ops));
    match &s.last_drain {
        Some(d) => {
            let commit = d.commit.as_deref().unwrap_or("sem commit");
            let push = match d.pushed {
                Some(true) => ", push ok",
                Some(false) => ", PUSH FALHOU",
                None => "",
            };
            out.push_str(&format!(
                "  último drain: {} — {} op(s), commit {}{}\n",
                format_ago(now, d.at),
                d.ops,
                &commit[..commit.len().min(8)],
                push
            ));
        }
        None => out.push_str("  último drain: ainda não houve\n"),
    }
    match &s.last_pull {
        Some(p) => out.push_str(&format!(
            "  último pull: {} — {}\n",
            format_ago(now, p.at),
            p.outcome
        )),
        None => out.push_str("  último pull: ainda não houve\n"),
    }
    if s.conflicts.is_empty() {
        out.push_str("  conflitos na sessão: nenhum\n");
    } else {
        out.push_str(&format!("  conflitos na sessão ({}):\n", s.conflicts.len()));
        for c in &s.conflicts {
            out.push_str(&format!("    - {}\n", c));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample() -> SyncStatus {
        SyncStatus {
            pid: 42,
            repo: "/repo".into(),
            branch: "main".into(),
            started_at: 1_000,
            updated_at: 1_950,
            pending_ops: 2,
            last_drain: Some(DrainInfo {
                at: 1_900,
                ops: 3,
                commit: Some("abcdef0123456789".into()),
                pushed: Some(true),
            }),
            last_pull: Some(PullInfo {
                at: 1_940,
                outcome: "sem novidades".into(),
            }),
            conflicts: vec![],
        }
    }

    #[test]
    fn format_status_fresh_daemon() {
        let text = format_status(&sample(), 2_000, Some(true));
        assert!(text.contains("rodando (pid 42)"), "{}", text);
        assert!(text.contains("ops pendentes: 2"), "{}", text);
        assert!(text.contains("3 op(s), commit abcdef01, push ok"), "{}", text);
        assert!(text.contains("último pull: há 60 s — sem novidades") || text.contains("último pull: há 1 min — sem novidades"), "{}", text);
        assert!(text.contains("conflitos na sessão: nenhum"), "{}", text);
        assert!(!text.contains("atenção"), "{}", text);
    }

    #[test]
    fn format_status_dead_daemon_and_staleness() {
        let mut s = sample();
        s.updated_at = 100;
        let dead = format_status(&s, 2_000, Some(false));
        assert!(dead.contains("POSSIVELMENTE PARADO"), "{}", dead);
        // Staleness só alerta com daemon vivo
        let stale_alive = format_status(&s, 2_000, Some(true));
        assert!(stale_alive.contains("atenção"), "{}", stale_alive);
    }

    #[test]
    fn format_status_before_any_drain_or_pull() {
        let s = SyncStatus {
            branch: "main".into(),
            ..Default::default()
        };
        let text = format_status(&s, 2_000, None);
        assert!(text.contains("último drain: ainda não houve"), "{}", text);
        assert!(text.contains("último pull: ainda não houve"), "{}", text);
    }

    #[test]
    fn write_atomic_and_read_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let status = SyncStatus {
            pid: 42,
            branch: "main".into(),
            last_drain: Some(DrainInfo {
                at: 100,
                ops: 3,
                commit: Some("abc123".into()),
                pushed: Some(true),
            }),
            ..Default::default()
        };
        write_atomic(&path, &status).unwrap();
        let loaded = read_from(&path).unwrap();
        assert_eq!(loaded.pid, 42);
        assert_eq!(loaded.branch, "main");
        assert_eq!(loaded.last_drain.unwrap().ops, 3);
    }

    #[test]
    fn read_from_missing_file_errors() {
        assert!(read_from(Path::new("/nonexistent/status.json")).is_err());
    }

    #[test]
    fn status_state_records_events_and_persists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("status.json");
        let state = StatusState::new();
        state.record_boot("/repo", "main");
        state.enable_file(path.clone());
        state.record_drain(2, Some("deadbeef".into()), Some(true), 0);
        state.record_pull("fast_forward");
        state.add_conflicts(vec!["a.txt.conflict-host-1".into()]);

        let loaded = read_from(&path).unwrap();
        assert_eq!(loaded.branch, "main");
        assert_eq!(loaded.last_drain.as_ref().unwrap().commit.as_deref(), Some("deadbeef"));
        assert_eq!(loaded.last_pull.as_ref().unwrap().outcome, "fast_forward");
        assert_eq!(loaded.conflicts, vec!["a.txt.conflict-host-1".to_string()]);
        assert!(loaded.updated_at >= loaded.started_at);
    }
}
