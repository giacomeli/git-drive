//! Resolução de paths de config, logs e status.
//!
//! Regra (decisão 10 da Fase 2): sem flag, tudo continua relativo ao CWD
//! (compatível com o fluxo dev `cargo run` a partir do projeto). Os paths
//! padrão do macOS só entram via flags explícitas — o `drive-sync install`
//! grava essas flags no plist do LaunchAgent, então o plist é a fonte da
//! verdade legível de onde estão config, log e status.

use std::path::{Path, PathBuf};

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// `~/Library/Application Support/drive-sync` (config + status no uso via launchd).
pub fn app_support_dir() -> PathBuf {
    app_support_dir_from(&home_dir())
}

pub(crate) fn app_support_dir_from(home: &Path) -> PathBuf {
    home.join("Library/Application Support/drive-sync")
}

/// `~/Library/Logs/drive-sync` (logs no uso via launchd).
pub fn standard_log_dir() -> PathBuf {
    standard_log_dir_from(&home_dir())
}

pub(crate) fn standard_log_dir_from(home: &Path) -> PathBuf {
    home.join("Library/Logs/drive-sync")
}

pub fn standard_config_path() -> PathBuf {
    app_support_dir().join("config.json")
}

pub fn standard_status_path() -> PathBuf {
    app_support_dir().join("status.json")
}

/// Path do status para LEITURA (subcomando `status` e tray):
/// flag explícita > `./logs/status.json` (fluxo dev) > padrão do macOS.
pub fn resolve_status_for_read(flag: Option<&str>) -> PathBuf {
    resolve_status_for_read_from(flag, Path::new("logs/status.json"), &standard_status_path())
}

fn resolve_status_for_read_from(flag: Option<&str>, local: &Path, standard: &Path) -> PathBuf {
    if let Some(f) = flag {
        return PathBuf::from(f);
    }
    if local.exists() {
        return local.to_path_buf();
    }
    standard.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn standard_dirs_derive_from_home() {
        let home = Path::new("/Users/teste");
        assert_eq!(
            app_support_dir_from(home),
            PathBuf::from("/Users/teste/Library/Application Support/drive-sync")
        );
        assert_eq!(
            standard_log_dir_from(home),
            PathBuf::from("/Users/teste/Library/Logs/drive-sync")
        );
    }

    #[test]
    fn resolve_status_flag_wins() {
        let out = resolve_status_for_read_from(
            Some("/x/status.json"),
            Path::new("/nao-existe/logs/status.json"),
            Path::new("/padrao/status.json"),
        );
        assert_eq!(out, PathBuf::from("/x/status.json"));
    }

    #[test]
    fn resolve_status_prefers_existing_local_then_standard() {
        let dir = tempdir().unwrap();
        let local = dir.path().join("status.json");
        let standard = Path::new("/padrao/status.json");

        // local não existe -> padrão
        assert_eq!(
            resolve_status_for_read_from(None, &local, standard),
            standard.to_path_buf()
        );
        // local existe -> local
        std::fs::write(&local, b"{}").unwrap();
        assert_eq!(resolve_status_for_read_from(None, &local, standard), local);
    }
}
