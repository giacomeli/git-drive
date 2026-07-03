use std::fs;
use std::path::Path;
use serde::Deserialize;

use crate::log_ts;

fn default_write_debounce_secs() -> u64 {
    5
}

fn default_pull_interval_secs() -> u64 {
    60
}

fn default_read_cache_mb() -> u64 {
    64
}

fn default_large_file_threshold_mb() -> u64 {
    64
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default = "default_write_debounce_secs")]
    pub write_debounce_secs: u64,
    #[serde(default = "default_pull_interval_secs")]
    pub pull_interval_secs: u64,
    /// Orçamento do cache LRU de blobs de leitura, em MiB. `0` desativa.
    #[serde(default = "default_read_cache_mb")]
    pub read_cache_mb: u64,
    /// Limiar em MiB acima do qual write buffers são mantidos em arquivo de
    /// spool em disco (pico de RAM limitado). `0` desativa o spool.
    #[serde(default = "default_large_file_threshold_mb")]
    pub large_file_threshold_mb: u64,
    #[serde(default)]
    pub user_name: Option<String>,
    #[serde(default)]
    pub user_email: Option<String>,
    #[serde(default)]
    pub remote_token: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            write_debounce_secs: default_write_debounce_secs(),
            pull_interval_secs: default_pull_interval_secs(),
            read_cache_mb: default_read_cache_mb(),
            large_file_threshold_mb: default_large_file_threshold_mb(),
            user_name: None,
            user_email: None,
            remote_token: None,
        }
    }
}

impl Config {
    pub fn load_from(path: &str) -> Self {
        if !Path::new(path).exists() {
            println!(
                "[{}] config: {} não encontrado, usando padrão",
                log_ts(),
                path
            );
            return Config::default();
        }

        let content = fs::read_to_string(path).unwrap_or_else(|_| {
            println!(
                "[{}] config: erro ao ler {}, usando padrão",
                log_ts(),
                path
            );
            "{}".to_string()
        });

        serde_json::from_str(&content).unwrap_or_else(|_| {
            println!(
                "[{}] config: erro ao parsear {}, usando padrão",
                log_ts(),
                path
            );
            Config::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_config(content: &str) -> (tempfile::TempDir, String) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(&path, content).unwrap();
        let path_str = path.to_str().unwrap().to_string();
        (dir, path_str)
    }

    #[test]
    fn load_from_missing_file_returns_defaults() {
        let cfg = Config::load_from("/nonexistent/path/config.json");
        assert_eq!(cfg.write_debounce_secs, 5);
        assert_eq!(cfg.pull_interval_secs, 60);
        assert!(cfg.user_name.is_none());
        assert!(cfg.user_email.is_none());
        assert!(cfg.remote_token.is_none());
    }

    #[test]
    fn load_from_empty_object_returns_defaults() {
        let (_dir, path) = write_config("{}");
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.write_debounce_secs, 5);
        assert_eq!(cfg.pull_interval_secs, 60);
        assert_eq!(cfg.read_cache_mb, 64);
        assert!(cfg.user_name.is_none());
        assert!(cfg.user_email.is_none());
        assert!(cfg.remote_token.is_none());
    }

    #[test]
    fn load_from_default_large_file_threshold_is_64() {
        let (_dir, path) = write_config("{}");
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.large_file_threshold_mb, 64);
    }

    #[test]
    fn load_from_large_file_threshold_override() {
        let (_dir, path) = write_config(r#"{"large_file_threshold_mb": 256}"#);
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.large_file_threshold_mb, 256);
    }

    #[test]
    fn load_from_read_cache_mb_zero_disables() {
        let (_dir, path) = write_config(r#"{"read_cache_mb": 0}"#);
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.read_cache_mb, 0);
    }

    #[test]
    fn load_from_partial_fields_fills_defaults() {
        let (_dir, path) = write_config(r#"{"write_debounce_secs": 10}"#);
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.write_debounce_secs, 10);
        assert_eq!(cfg.pull_interval_secs, 60);
        assert!(cfg.user_name.is_none());
    }

    #[test]
    fn load_from_all_fields() {
        let (_dir, path) = write_config(
            r#"{
                "write_debounce_secs": 3,
                "pull_interval_secs": 30,
                "user_name": "Alice",
                "user_email": "alice@example.com",
                "remote_token": "secret"
            }"#,
        );
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.write_debounce_secs, 3);
        assert_eq!(cfg.pull_interval_secs, 30);
        assert_eq!(cfg.user_name.as_deref(), Some("Alice"));
        assert_eq!(cfg.user_email.as_deref(), Some("alice@example.com"));
        assert_eq!(cfg.remote_token.as_deref(), Some("secret"));
    }

    #[test]
    fn load_from_invalid_json_returns_defaults() {
        let (_dir, path) = write_config("not json at all");
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.write_debounce_secs, 5);
        assert_eq!(cfg.pull_interval_secs, 60);
    }
}