//! Notificações do macOS via `osascript` (decisão 3 da Fase 2 — zero
//! dependências, sem exigir app bundle). Eventos persistentes usam rate limit
//! (decisão 12): primeira ocorrência + no máximo uma por hora enquanto a
//! condição durar; a condição resolvida zera o limitador.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::status::now_unix;

/// Janela do rate limit para eventos persistentes (1 hora).
const RATE_LIMIT_SECS: u64 = 3600;

pub const KEY_PULL_BLOCKED: &str = "pull_blocked";

#[derive(Debug, Clone)]
pub enum NotifyEvent {
    /// Merge criou conflicted copies — o usuário precisa resolver.
    Conflicts { copies: Vec<String> },
    /// Push + reconcile falharam — commits locais não publicados.
    PushFailed,
    /// Volume montado e pronto.
    Mounted { mount: String },
    /// Parada limpa (SIGTERM/eject) concluída.
    Stopped,
    /// Pull pulado por working tree sujo (persistente, rate-limited).
    PullBlocked,
}

impl NotifyEvent {
    /// Eventos persistentes têm chave de rate limit; pontuais são None.
    fn persistent_key(&self) -> Option<&'static str> {
        match self {
            NotifyEvent::PullBlocked => Some(KEY_PULL_BLOCKED),
            _ => None,
        }
    }
}

/// Título e corpo da notificação (pt-BR). Pura e testável.
pub fn format_event(event: &NotifyEvent) -> (String, String) {
    match event {
        NotifyEvent::Conflicts { copies } => {
            let list = copies
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if copies.len() > 3 {
                format!(" (+{})", copies.len() - 3)
            } else {
                String::new()
            };
            (
                "drive-sync: conflito de merge".to_string(),
                format!(
                    "Versão local preservada em {}{}. A remota virou canônica.",
                    list, more
                ),
            )
        }
        NotifyEvent::PushFailed => (
            "drive-sync: push falhou".to_string(),
            "Mudanças commitadas localmente mas não publicadas. Verifique rede/credenciais (drive-sync status).".to_string(),
        ),
        NotifyEvent::Mounted { mount } => (
            "drive-sync: drive montado".to_string(),
            format!("Volume disponível em {}.", mount),
        ),
        NotifyEvent::Stopped => (
            "drive-sync: drive desmontado".to_string(),
            "Parada limpa concluída — nada ficou pendente.".to_string(),
        ),
        NotifyEvent::PullBlocked => (
            "drive-sync: pull bloqueado".to_string(),
            "Mudanças locais não commitadas impedem o pull (modo sem identidade?). O sync está unidirecional.".to_string(),
        ),
    }
}

/// Rate limiter por chave: dispara na primeira ocorrência e depois de
/// `window_secs` enquanto a condição persistir. `reset` ao resolver.
pub struct RateLimiter {
    window_secs: u64,
    last_fired: Mutex<HashMap<&'static str, u64>>,
}

impl RateLimiter {
    pub fn new(window_secs: u64) -> Self {
        RateLimiter {
            window_secs,
            last_fired: Mutex::new(HashMap::new()),
        }
    }

    pub fn should_fire(&self, key: &'static str, now: u64) -> bool {
        let mut map = self.last_fired.lock();
        match map.get(key) {
            Some(last) if now.saturating_sub(*last) < self.window_secs => false,
            _ => {
                map.insert(key, now);
                true
            }
        }
    }

    pub fn reset(&self, key: &'static str) {
        self.last_fired.lock().remove(key);
    }
}

/// Despachante de notificações. Nasce DESLIGADO — testes e usos embutidos não
/// notificam; o modo mount real chama `enable()`.
pub struct Notifier {
    enabled: AtomicBool,
    limiter: RateLimiter,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier {
    pub fn new() -> Self {
        Notifier {
            enabled: AtomicBool::new(false),
            limiter: RateLimiter::new(RATE_LIMIT_SECS),
        }
    }

    pub fn enable(&self) {
        self.enabled.store(true, Ordering::SeqCst);
    }

    /// Condição persistente resolvida — a próxima ocorrência notifica de novo.
    pub fn reset(&self, key: &'static str) {
        self.limiter.reset(key);
    }

    pub fn notify(&self, event: NotifyEvent) {
        if !self.enabled.load(Ordering::SeqCst) {
            return;
        }
        if let Some(key) = event.persistent_key() {
            if !self.limiter.should_fire(key, now_unix()) {
                return;
            }
        }
        let (title, body) = format_event(&event);
        dispatch_osascript(&title, &body);
    }
}

fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn dispatch_osascript(title: &str, body: &str) {
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_escape(body),
        applescript_escape(title)
    );
    // Fire-and-forget: falha de notificação nunca pode afetar o sync.
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_conflicts_truncates_list() {
        let copies: Vec<String> = (0..5).map(|i| format!("f{}.conflict-h-1", i)).collect();
        let (title, body) = format_event(&NotifyEvent::Conflicts { copies });
        assert!(title.contains("conflito"));
        assert!(body.contains("f0.conflict-h-1"));
        assert!(body.contains("(+2)"), "{}", body);
    }

    #[test]
    fn rate_limiter_first_fires_then_blocks_within_window() {
        let rl = RateLimiter::new(3600);
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 1_000));
        assert!(!rl.should_fire(KEY_PULL_BLOCKED, 1_500));
        assert!(!rl.should_fire(KEY_PULL_BLOCKED, 4_599));
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 4_600), "janela expirada deve disparar");
    }

    #[test]
    fn rate_limiter_reset_rearms_immediately() {
        let rl = RateLimiter::new(3600);
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 1_000));
        rl.reset(KEY_PULL_BLOCKED);
        assert!(rl.should_fire(KEY_PULL_BLOCKED, 1_001), "reset deve rearmar");
    }

    #[test]
    fn notifier_disabled_never_rate_limits() {
        // Desligado não consome a janela do limiter (não notifica nem marca).
        let n = Notifier::new();
        n.notify(NotifyEvent::PullBlocked);
        assert!(
            n.limiter.should_fire(KEY_PULL_BLOCKED, now_unix()),
            "evento com notifier desligado não deve consumir a janela"
        );
    }

    #[test]
    fn applescript_escape_handles_quotes() {
        assert_eq!(applescript_escape(r#"a "b" \c"#), r#"a \"b\" \\c"#);
    }
}
