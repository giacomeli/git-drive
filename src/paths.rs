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

/// `~/.config/drive-sync` — diretório canônico de config + chave (decisão do
/// usuário em 06-07-2026; também alinha com o XDG do futuro port Linux).
pub fn config_dir() -> PathBuf {
    config_dir_from(&home_dir())
}

pub(crate) fn config_dir_from(home: &Path) -> PathBuf {
    home.join(".config/drive-sync")
}

pub fn standard_config_path() -> PathBuf {
    standard_config_path_from(&home_dir())
}

pub(crate) fn standard_config_path_from(home: &Path) -> PathBuf {
    config_dir_from(home).join("config.json")
}

/// Path padrão do arquivo de chave da criptografia (Fase 2.7).
pub fn standard_key_path() -> PathBuf {
    standard_key_path_from(&home_dir())
}

pub(crate) fn standard_key_path_from(home: &Path) -> PathBuf {
    config_dir_from(home).join("drive.key")
}

/// Locais antigos (pré 06-07-2026), mantidos como fallback de LEITURA para
/// instalações existentes.
pub fn legacy_config_path() -> PathBuf {
    app_support_dir().join("config.json")
}

pub fn legacy_key_path() -> PathBuf {
    app_support_dir().join("drive.key")
}

/// Config para LEITURA: flag explícita > `./config.json` (fluxo dev) >
/// `~/.config/drive-sync/config.json` > local legado (Application Support).
/// Sem nenhum existente, o default é o path XDG (a mensagem de "não
/// encontrado" aponta para o lugar canônico).
pub fn resolve_config_for_read(flag: Option<&str>) -> PathBuf {
    resolve_config_for_read_from(
        flag,
        Path::new("config.json"),
        &standard_config_path(),
        &legacy_config_path(),
    )
}

fn resolve_config_for_read_from(
    flag: Option<&str>,
    dev: &Path,
    xdg: &Path,
    legacy: &Path,
) -> PathBuf {
    if let Some(f) = flag {
        return PathBuf::from(f);
    }
    for candidate in [dev, xdg, legacy] {
        if candidate.exists() {
            return candidate.to_path_buf();
        }
    }
    xdg.to_path_buf()
}

/// Chave para LEITURA: valor explícito do config > XDG > local legado.
pub fn resolve_key_for_read(explicit: Option<&str>) -> PathBuf {
    resolve_key_for_read_from(explicit, &standard_key_path(), &legacy_key_path())
}

fn resolve_key_for_read_from(explicit: Option<&str>, xdg: &Path, legacy: &Path) -> PathBuf {
    if let Some(e) = explicit {
        return PathBuf::from(e);
    }
    if xdg.exists() {
        return xdg.to_path_buf();
    }
    if legacy.exists() {
        return legacy.to_path_buf();
    }
    xdg.to_path_buf()
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
        assert_eq!(
            config_dir_from(home),
            PathBuf::from("/Users/teste/.config/drive-sync")
        );
    }

    #[test]
    fn config_and_key_defaults_live_in_xdg_config_dir() {
        let home = Path::new("/Users/teste");
        assert_eq!(
            standard_config_path_from(home),
            PathBuf::from("/Users/teste/.config/drive-sync/config.json")
        );
        assert_eq!(
            standard_key_path_from(home),
            PathBuf::from("/Users/teste/.config/drive-sync/drive.key")
        );
    }

    #[test]
    fn resolve_config_order_flag_dev_xdg_legacy() {
        let dir = tempdir().unwrap();
        let dev = dir.path().join("config.json");
        let xdg = dir.path().join("xdg/config.json");
        let legacy = dir.path().join("legacy/config.json");

        // flag vence tudo
        assert_eq!(
            resolve_config_for_read_from(Some("/x/c.json"), &dev, &xdg, &legacy),
            PathBuf::from("/x/c.json")
        );
        // nada existe -> default é o XDG (a mensagem de "não encontrado"
        // aponta para o lugar canônico novo)
        assert_eq!(
            resolve_config_for_read_from(None, &dev, &xdg, &legacy),
            xdg
        );
        // legacy existe -> legacy (migração suave)
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"{}").unwrap();
        assert_eq!(
            resolve_config_for_read_from(None, &dev, &xdg, &legacy),
            legacy
        );
        // XDG existe -> XDG vence o legacy
        std::fs::create_dir_all(xdg.parent().unwrap()).unwrap();
        std::fs::write(&xdg, b"{}").unwrap();
        assert_eq!(resolve_config_for_read_from(None, &dev, &xdg, &legacy), xdg);
        // dev (CWD) existe -> dev vence (fluxo cargo run no projeto)
        std::fs::write(&dev, b"{}").unwrap();
        assert_eq!(resolve_config_for_read_from(None, &dev, &xdg, &legacy), dev);
    }

    #[test]
    fn resolve_key_order_explicit_xdg_legacy() {
        let dir = tempdir().unwrap();
        let xdg = dir.path().join("xdg/drive.key");
        let legacy = dir.path().join("legacy/drive.key");

        assert_eq!(
            resolve_key_for_read_from(Some("/k/drive.key"), &xdg, &legacy),
            PathBuf::from("/k/drive.key")
        );
        assert_eq!(resolve_key_for_read_from(None, &xdg, &legacy), xdg);
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"k").unwrap();
        assert_eq!(resolve_key_for_read_from(None, &xdg, &legacy), legacy);
        std::fs::create_dir_all(xdg.parent().unwrap()).unwrap();
        std::fs::write(&xdg, b"k").unwrap();
        assert_eq!(resolve_key_for_read_from(None, &xdg, &legacy), xdg);
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
