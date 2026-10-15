//! Boot glue of the channel (feature `p2p`): decides whether it runs, loads
//! the device key, registers the device, starts the node and plugs it into
//! the `SyncManager`. Every failure here leaves the drive on git polling —
//! the channel is an accelerator, never a requirement.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::node::{NodeConfig, PeerNode, Relays};
use super::registry;
use crate::config::Config;
use crate::crypt::Crypto;
use crate::status::{P2pInfo, PeerInfo, StatusState};
use crate::sync::{P2pHooks, SyncManager};

/// Where the device key lives: `p2p_key_file`, or `peer.key` next to the
/// config file in use (each config dir is one device, which keeps several
/// clients on one machine apart).
pub fn key_path(config: &Config, config_path: &Path) -> PathBuf {
    match &config.p2p_key_file {
        Some(path) => PathBuf::from(path),
        None => config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("peer.key"),
    }
}

/// Name of this device in the registry: `p2p_device_name`, or the short
/// hostname.
pub fn device_name(config: &Config) -> String {
    config.p2p_device_name.clone().unwrap_or_else(short_hostname)
}

fn short_hostname() -> String {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let full = if ok { String::from_utf8_lossy(&buf[..len]).into_owned() } else { String::new() };
    match full.split('.').next() {
        Some(short) if !short.is_empty() => short.to_string(),
        _ => "device".to_string(),
    }
}

/// Why the channel stays off, for the boot log. `None` = it should start.
pub fn skip_reason(config: &Config) -> Option<&'static str> {
    if !config.p2p_enabled {
        return Some("disabled in config");
    }
    if config.user_name.is_none() || config.user_email.is_none() {
        return Some("no commit identity (user_name/user_email): the device cannot register");
    }
    None
}

pub struct ChannelSettings<'a> {
    pub config: &'a Config,
    pub config_path: &'a Path,
    pub repo_path: &'a Path,
    pub branch: &'a str,
    pub crypto: Option<Arc<Crypto>>,
    /// Overrides the relays from the config (tests use `Relays::Disabled`).
    pub relays: Option<Relays>,
}

/// A running channel. Dropping it does not stop the node: call `shutdown`.
pub struct Channel {
    node: PeerNode,
}

impl Channel {
    pub fn node(&self) -> &PeerNode {
        &self.node
    }

    pub async fn shutdown(&self) {
        self.node.shutdown().await;
    }
}

/// Starts the channel when the config asks for it and plugs it into `sync`.
/// `None` = the drive runs on git polling only; the reason is logged.
pub async fn start(settings: ChannelSettings<'_>, sync: &SyncManager) -> Option<Channel> {
    if let Some(reason) = skip_reason(settings.config) {
        log::info!("p2p: off ({})", reason);
        return None;
    }
    let config = settings.config;
    let key_file = key_path(config, settings.config_path);
    let secret = match super::identity::load_or_create(&key_file) {
        Ok(secret) => secret,
        Err(e) => {
            log::error!("p2p: off — device key {}: {}", key_file.display(), e);
            return None;
        }
    };
    let id = super::node::device_id(&secret);
    let name = device_name(config);
    let identity = (
        config.user_name.as_deref().unwrap_or_default(),
        config.user_email.as_deref().unwrap_or_default(),
    );
    {
        let _repo = sync.lock_repo();
        match registry::register_self(settings.repo_path, &id, &name, settings.crypto.as_deref(), identity) {
            Ok(true) => log::info!("p2p: device {} registered as {:?}", &id[..8], name),
            Ok(false) => {}
            // Unregistered, the other devices refuse us; the drive still
            // works, and the next boot tries again.
            Err(e) => log::warn!("p2p: could not register this device: {}", e),
        }
    }

    let relays = settings.relays.clone().unwrap_or_else(|| match &config.p2p_relay_url {
        Some(url) => Relays::Custom(url.clone()),
        None => Relays::N0,
    });
    let online = Arc::new(AtomicBool::new(false));
    let node_cfg = NodeConfig {
        secret,
        repo_path: settings.repo_path.to_path_buf(),
        branch: settings.branch.to_string(),
        relays,
    };
    let node = match PeerNode::start(node_cfg, sync.pull_waker(), online.clone()).await {
        Ok(node) => node,
        Err(e) => {
            log::error!("p2p: off — endpoint failed to start: {}", e);
            return None;
        }
    };

    let names: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let reload = {
        let node = node.clone();
        let names = names.clone();
        let repo_path = settings.repo_path.to_path_buf();
        let crypto = settings.crypto.clone();
        Arc::new(move || {
            let entries = git2::Repository::open(&repo_path)
                .and_then(|repo| registry::list(&repo, crypto.as_deref()));
            match entries {
                Ok(entries) => {
                    let ids: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
                    *names.lock() = entries.into_iter().map(|e| (e.id, e.name)).collect();
                    node.set_registry(&ids);
                }
                Err(e) => log::warn!("p2p: could not read the device registry: {}", e),
            }
        })
    };
    reload();

    let announcer = node.clone();
    sync.set_p2p(P2pHooks {
        online: online.clone(),
        idle_pull_interval_secs: config.p2p_idle_pull_interval_secs,
        on_pushed: Arc::new(move |commit| announcer.announce(commit)),
        on_head_changed: reload,
    });
    spawn_status_publisher(node.clone(), sync.status_state(), names, id, name, online);
    Some(Channel { node })
}

/// The status file is rewritten at least this often while the channel runs:
/// the tray reads a status older than 180 s as a stopped daemon, and with the
/// channel online the pull (which used to refresh it every minute) runs only
/// every `p2p_idle_pull_interval_secs`.
pub const STATUS_HEARTBEAT_SECS: u64 = 60;

fn should_publish(changed: bool, since_last_write_secs: u64) -> bool {
    changed || since_last_write_secs >= STATUS_HEARTBEAT_SECS
}

/// Publishes the channel state to the status file when it changes, and at
/// least every `STATUS_HEARTBEAT_SECS` (heartbeat).
fn spawn_status_publisher(
    node: PeerNode,
    status: Arc<StatusState>,
    names: Arc<Mutex<HashMap<String, String>>>,
    device_id: String,
    device_name: String,
    online: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let mut last: Option<P2pInfo> = None;
        let mut last_write = std::time::Instant::now();
        loop {
            let names = names.lock().clone();
            let info = P2pInfo {
                online: online.load(Ordering::SeqCst),
                device_id: device_id.clone(),
                device_name: device_name.clone(),
                peers: node
                    .peers()
                    .into_iter()
                    .map(|p| PeerInfo {
                        name: names.get(&p.id).cloned().unwrap_or_default(),
                        id: p.id,
                        online: p.online,
                        last_seen: p.last_seen,
                    })
                    .collect(),
            };
            // Last-seen times of connected peers move every tick; only a
            // change in who is online (or the registry) rewrites the file.
            let shape = |i: &P2pInfo| {
                (i.online, i.peers.iter().map(|p| (p.id.clone(), p.name.clone(), p.online)).collect::<Vec<_>>())
            };
            let changed = last.as_ref().map(shape) != Some(shape(&info));
            if should_publish(changed, last_write.elapsed().as_secs()) {
                status.set_p2p(Some(info.clone()));
                last = Some(info);
                last_write = std::time::Instant::now();
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_defaults_next_to_the_config_in_use() {
        let config = Config::default();
        assert_eq!(
            key_path(&config, Path::new("/home/u/.config/git-drive/config.json")),
            PathBuf::from("/home/u/.config/git-drive/peer.key")
        );
        let config = Config { p2p_key_file: Some("/k/dev.key".into()), ..Config::default() };
        assert_eq!(key_path(&config, Path::new("/x/config.json")), PathBuf::from("/k/dev.key"));
    }

    #[test]
    fn the_name_defaults_to_the_short_hostname() {
        let name = device_name(&Config::default());
        assert!(!name.is_empty() && !name.contains('.'), "{:?}", name);
        let config = Config { p2p_device_name: Some("mac".into()), ..Config::default() };
        assert_eq!(device_name(&config), "mac");
    }

    #[test]
    fn the_status_is_rewritten_on_change_and_at_least_every_minute() {
        // The tray reads a status older than 180 s as a stopped daemon; with
        // the channel online the pull (which used to refresh it every
        // minute) runs only every 10 minutes.
        assert!(should_publish(true, 0), "a change is published at once");
        assert!(!should_publish(false, 30), "nothing new, written recently");
        assert!(should_publish(false, STATUS_HEARTBEAT_SECS), "heartbeat");
    }

    #[test]
    fn the_channel_needs_the_flag_and_a_commit_identity() {
        assert!(skip_reason(&Config::default()).is_some(), "off by default");
        let no_identity = Config { p2p_enabled: true, ..Config::default() };
        assert!(skip_reason(&no_identity).unwrap().contains("identity"));
        let ready = Config {
            p2p_enabled: true,
            user_name: Some("A".into()),
            user_email: Some("a@x.com".into()),
            ..Config::default()
        };
        assert_eq!(skip_reason(&ready), None);
    }
}
