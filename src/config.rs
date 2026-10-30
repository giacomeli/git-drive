use std::fs;
use std::path::Path;
use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

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

fn default_chunk_threshold_mb() -> u64 {
    95
}

fn default_chunk_size_mb() -> u64 {
    80
}

fn default_p2p_idle_pull_interval_secs() -> u64 {
    600
}

fn default_encryption_enabled() -> bool {
    false
}

fn default_commit_batch_mb() -> u64 {
    256
}

fn default_log_max_mb() -> u64 {
    50
}

fn default_log_keep_files() -> usize {
    10
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_git_transport() -> String {
    "auto".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default = "default_write_debounce_secs")]
    pub write_debounce_secs: u64,
    #[serde(default = "default_pull_interval_secs")]
    pub pull_interval_secs: u64,
    /// Budget for the read blob LRU cache, in MiB. `0` disables it.
    #[serde(default = "default_read_cache_mb")]
    pub read_cache_mb: u64,
    /// Threshold in MiB above which write buffers are kept in an on-disk
    /// spool file (bounded RAM peak). `0` disables the spool.
    #[serde(default = "default_large_file_threshold_mb")]
    pub large_file_threshold_mb: u64,
    /// Threshold in MiB above which a file is converted into a manifest +
    /// chunks (`.chunks/`) at commit time — no blob above it is staged.
    /// Default sits below GitHub's 100 MB per-file limit. `0` disables it.
    #[serde(default = "default_chunk_threshold_mb")]
    pub chunk_threshold_mb: u64,
    /// Maximum size of each chunk, in MiB.
    #[serde(default = "default_chunk_size_mb")]
    pub chunk_size_mb: u64,
    /// Maximum content bytes (in MiB) per drain batch: the causal queue is
    /// split into size-bounded batches, each becoming its own commit, and
    /// commits are published one by one (bounded push packs — a single 24 GB
    /// pack is what GitHub aborted in the 2026-07-10 incident). A single file
    /// larger than the limit forms a batch of its own. `0` disables batching
    /// (whole drain = one commit).
    #[serde(default = "default_commit_batch_mb")]
    pub commit_batch_mb: u64,
    /// At-rest encryption (Phase 2.7): content and names encrypted in the repo.
    /// Enabling/disabling triggers an automatic migration on the next drain.
    /// WARNING: Git history from before enabling remains in cleartext.
    #[serde(default = "default_encryption_enabled")]
    pub encryption_enabled: bool,
    /// File holding the master key (64 hex, permission 0600). Absent = the
    /// platform's default path. Generate it with `git-drive key generate`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption_key_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_token: Option<String>,
    /// Maximum size of a single session log file, in MiB, before rotating to a
    /// new file. Keeps the log from growing without bound (see the 225 MB
    /// incident). `0` disables size-based rotation.
    #[serde(default = "default_log_max_mb")]
    pub log_max_mb: u64,
    /// How many rotated log files to keep; older ones are pruned automatically.
    #[serde(default = "default_log_keep_files")]
    pub log_keep_files: usize,
    /// Default log level (`error`/`warn`/`info`/`debug`/`trace`). Overridable
    /// at runtime by the `RUST_LOG` environment variable.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Per-category log levels (`sync`, `git`, `fuse`, `crypto`, `p2p`,
    /// `network`, `service` — see `logview::CATEGORIES`), each `off` to
    /// `trace`. Unset categories follow `log_level`, except `network` (the
    /// iroh stack), which defaults to `warn`. `RUST_LOG` overrides it all.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub log_categories: BTreeMap<String, String>,
    /// Network transport for fetch/push: `auto` (system `git` when available,
    /// libgit2 otherwise), `cli` or `libgit2`. The CLI authenticates exactly
    /// like a manual `git fetch` (OpenSSH config, agent, Keychain, credential
    /// helpers); libgit2 only knows the agent and `~/.ssh/id_*`.
    #[serde(default = "default_git_transport")]
    pub git_transport: String,
    /// P2P notification channel between the devices of the drive (opt-in per
    /// machine). Off = the drive syncs only through git polling, as before.
    #[serde(default)]
    pub p2p_enabled: bool,
    /// Pull interval while the P2P channel is online: pushes arrive as
    /// notifications, so polling is only the safety net.
    #[serde(default = "default_p2p_idle_pull_interval_secs")]
    pub p2p_idle_pull_interval_secs: u64,
    /// Device secret key. Absent = `peer.key` next to the config file in use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_key_file: Option<String>,
    /// Name of this device in the registry. Absent = short hostname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_device_name: Option<String>,
    /// Own relay server. Absent = the public n0 relays and DNS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p2p_relay_url: Option<String>,
    /// How much the repository may hold, in decimal GB, shown against its
    /// size. Absent = the host's (GitHub 10, GitLab.com 10; none elsewhere).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_quota_gb: Option<u64>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            write_debounce_secs: default_write_debounce_secs(),
            pull_interval_secs: default_pull_interval_secs(),
            read_cache_mb: default_read_cache_mb(),
            large_file_threshold_mb: default_large_file_threshold_mb(),
            chunk_threshold_mb: default_chunk_threshold_mb(),
            chunk_size_mb: default_chunk_size_mb(),
            commit_batch_mb: default_commit_batch_mb(),
            encryption_enabled: false,
            encryption_key_file: None,
            user_name: None,
            user_email: None,
            remote_token: None,
            log_max_mb: default_log_max_mb(),
            log_keep_files: default_log_keep_files(),
            log_level: default_log_level(),
            log_categories: BTreeMap::new(),
            git_transport: default_git_transport(),
            p2p_enabled: false,
            p2p_idle_pull_interval_secs: default_p2p_idle_pull_interval_secs(),
            p2p_key_file: None,
            p2p_device_name: None,
            p2p_relay_url: None,
            repo_quota_gb: None,
        }
    }
}

impl Config {
    /// Strict loader for the mount mode: a missing file means defaults, but a
    /// file that exists and does not parse is an ERROR. Silently falling back
    /// to defaults there would boot with encryption OFF (and, with the key at
    /// its default path, migrate the repo back to cleartext and push it) or
    /// without an identity — a typo must stop the daemon, not leak the drive.
    pub fn try_load_from(path: &str) -> Result<Self, String> {
        if !Path::new(path).exists() {
            eprintln!("config: {} not found, using defaults", path);
            return Ok(Config::default());
        }
        let content = fs::read_to_string(path)
            .map_err(|e| format!("failed to read config {}: {}", path, e))?;
        serde_json::from_str(&content)
            .map_err(|e| format!("invalid config {}: {}", path, e))
    }

    /// Writes the config to `path` atomically (temp file + rename) with mode
    /// 0600 — it can hold `remote_token`. Keys of the existing file that this
    /// version does not know are kept; unset optional fields are removed.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut doc = match fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            Some(serde_json::Value::Object(existing)) => existing,
            _ => serde_json::Map::new(),
        };
        let serde_json::Value::Object(fields) = serde_json::to_value(self)? else {
            anyhow::bail!("config does not serialize to an object");
        };
        let known: Vec<String> = serde_json::to_value(Config {
            encryption_key_file: Some(String::new()),
            user_name: Some(String::new()),
            user_email: Some(String::new()),
            remote_token: Some(String::new()),
            p2p_key_file: Some(String::new()),
            p2p_device_name: Some(String::new()),
            p2p_relay_url: Some(String::new()),
            repo_quota_gb: Some(0),
            log_categories: BTreeMap::from([(String::new(), String::new())]),
            ..Config::default()
        })?
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
        for key in known {
            doc.remove(&key);
        }
        doc.extend(fields);
        let text = serde_json::to_string_pretty(&serde_json::Value::Object(doc))? + "\n";
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load_from(path: &str) -> Self {
        // Config loads BEFORE the logger is initialized (the logger's rotation
        // and level come from it), so these bootstrap diagnostics go straight
        // to stderr (captured by the journal under systemd), not the `log`
        // facade — which would silently drop them at this point.
        if !Path::new(path).exists() {
            eprintln!("config: {} not found, using defaults", path);
            return Config::default();
        }

        let content = fs::read_to_string(path).unwrap_or_else(|_| {
            eprintln!("config: failed to read {}, using defaults", path);
            "{}".to_string()
        });

        serde_json::from_str(&content).unwrap_or_else(|_| {
            eprintln!("config: failed to parse {}, using defaults", path);
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
    fn a_repository_quota_is_optional() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        assert_eq!(Config::default().repo_quota_gb, None, "the host's limit applies");
        std::fs::write(&path, r#"{"repo_quota_gb": 50}"#).unwrap();
        assert_eq!(Config::try_load_from(path.to_str().unwrap()).unwrap().repo_quota_gb, Some(50));
        let saved = serde_json::to_value(Config::default()).unwrap();
        assert!(saved.get("repo_quota_gb").is_none(), "absent from a saved config until set");
        // Cleared in the Control Center: the host's limit applies again.
        Config::default().save_to(&path).unwrap();
        assert_eq!(Config::try_load_from(path.to_str().unwrap()).unwrap().repo_quota_gb, None);
    }

    #[test]
    fn save_to_round_trips_atomically_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = Config { user_name: Some("A".into()), remote_token: Some("secret".into()), ..Config::default() };
        cfg.log_categories.insert("p2p".into(), "debug".into());
        cfg.save_to(&path).unwrap();
        let back = Config::try_load_from(path.to_str().unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&cfg).unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the config can hold a token");
        assert!(!dir.path().join("config.json.tmp").exists(), "no temp file left");
    }

    #[test]
    fn p2p_is_off_by_default_and_parses_when_set() {
        let (_dir, path) = write_config("{}");
        let cfg = Config::try_load_from(&path).unwrap();
        assert!(!cfg.p2p_enabled, "the channel is opt-in per machine");
        assert_eq!(cfg.p2p_idle_pull_interval_secs, 600);
        assert!(cfg.p2p_key_file.is_none() && cfg.p2p_device_name.is_none());
        assert!(cfg.p2p_relay_url.is_none());

        let (_dir, path) = write_config(
            r#"{"p2p_enabled": true, "p2p_idle_pull_interval_secs": 900,
                "p2p_key_file": "/k/peer.key", "p2p_device_name": "mac",
                "p2p_relay_url": "https://relay.example.com"}"#,
        );
        let cfg = Config::try_load_from(&path).unwrap();
        assert!(cfg.p2p_enabled);
        assert_eq!(cfg.p2p_idle_pull_interval_secs, 900);
        assert_eq!(cfg.p2p_key_file.as_deref(), Some("/k/peer.key"));
        assert_eq!(cfg.p2p_device_name.as_deref(), Some("mac"));
        assert_eq!(cfg.p2p_relay_url.as_deref(), Some("https://relay.example.com"));
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
    fn try_load_from_rejects_unparseable_file_but_accepts_missing() {
        let (_dir, path) = write_config(r#"{"encryption_enabled": true,,}"#);
        assert!(Config::try_load_from(&path).is_err());
        let missing = Config::try_load_from("/nonexistent/path/config.json").unwrap();
        assert!(!missing.encryption_enabled);
        let (_dir2, ok) = write_config(r#"{"encryption_enabled": true}"#);
        assert!(Config::try_load_from(&ok).unwrap().encryption_enabled);
    }

    #[test]
    fn git_transport_defaults_to_auto() {
        let (_dir, path) = write_config("{}");
        assert_eq!(Config::load_from(&path).git_transport, "auto");
        let (_dir2, path2) = write_config(r#"{"git_transport": "libgit2"}"#);
        assert_eq!(Config::load_from(&path2).git_transport, "libgit2");
    }

    #[test]
    fn load_from_encryption_defaults_off() {
        let (_dir, path) = write_config("{}");
        let cfg = Config::load_from(&path);
        assert!(!cfg.encryption_enabled);
        assert!(cfg.encryption_key_file.is_none());
    }

    #[test]
    fn load_from_encryption_overrides() {
        let (_dir, path) =
            write_config(r#"{"encryption_enabled": true, "encryption_key_file": "/x/drive.key"}"#);
        let cfg = Config::load_from(&path);
        assert!(cfg.encryption_enabled);
        assert_eq!(cfg.encryption_key_file.as_deref(), Some("/x/drive.key"));
    }

    #[test]
    fn load_from_default_chunking_fields() {
        let (_dir, path) = write_config("{}");
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.chunk_threshold_mb, 95);
        assert_eq!(cfg.chunk_size_mb, 80);
    }

    #[test]
    fn load_from_chunking_overrides_and_disable() {
        let (_dir, path) = write_config(r#"{"chunk_threshold_mb": 0, "chunk_size_mb": 40}"#);
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.chunk_threshold_mb, 0, "0 disables chunking");
        assert_eq!(cfg.chunk_size_mb, 40);
    }

    #[test]
    fn load_from_default_commit_batch_is_256_and_overridable() {
        let (_dir, path) = write_config("{}");
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.commit_batch_mb, 256);

        let (_dir2, path2) = write_config(r#"{"commit_batch_mb": 0}"#);
        let cfg2 = Config::load_from(&path2);
        assert_eq!(cfg2.commit_batch_mb, 0, "0 disables batching");
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
    fn load_from_default_log_fields() {
        let (_dir, path) = write_config("{}");
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.log_max_mb, 50);
        assert_eq!(cfg.log_keep_files, 10);
        assert_eq!(cfg.log_level, "info");
    }

    #[test]
    fn load_from_log_overrides() {
        let (_dir, path) =
            write_config(r#"{"log_max_mb": 10, "log_keep_files": 3, "log_level": "debug"}"#);
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.log_max_mb, 10);
        assert_eq!(cfg.log_keep_files, 3);
        assert_eq!(cfg.log_level, "debug");
    }

    #[test]
    fn load_from_invalid_json_returns_defaults() {
        let (_dir, path) = write_config("not json at all");
        let cfg = Config::load_from(&path);
        assert_eq!(cfg.write_debounce_secs, 5);
        assert_eq!(cfg.pull_interval_secs, 60);
    }
}