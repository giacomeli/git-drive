//! End-to-end flow of the P2P notification channel: two clients of one bare
//! remote, the channel on loopback (no relay, no lookup — the suite never
//! touches the network). A write on A reaches B in seconds although B polls
//! only once an hour.
#![cfg(feature = "p2p")]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{file_exists, init_bare_remote, init_repo};
use git_drive::config::Config;
use git_drive::peer::channel::{self, Channel, ChannelSettings};
use git_drive::peer::node::{device_id, Relays};
use git_drive::peer::registry;
use git_drive::sync::SyncManager;
use git2::Repository;

const SEED_A: [u8; 32] = [1; 32];
const SEED_B: [u8; 32] = [2; 32];

struct Client {
    _dir: tempfile::TempDir,
    _config_dir: tempfile::TempDir,
    path: PathBuf,
    sync: SyncManager,
    channel: Option<Channel>,
}

/// Bare remote whose history already lists both devices (the steady state:
/// each device registered on its first boot long ago).
fn remote_with_both_devices() -> tempfile::TempDir {
    let bare = init_bare_remote();
    let setup = init_repo();
    let url = format!("file://{}", bare.path().to_str().unwrap());
    setup.repo.remote("origin", &url).unwrap();
    let identity = ("Setup", "s@x.com");
    registry::register_self(setup.dir.path(), &device_id(&SEED_A), "a", None, identity).unwrap();
    registry::register_self(setup.dir.path(), &device_id(&SEED_B), "b", None, identity).unwrap();
    git_drive::git::push_to_remote(&setup.repo, "main", "origin", None).unwrap();
    bare
}

fn write_key(dir: &Path, seed: [u8; 32]) -> PathBuf {
    let path = dir.join("peer.key");
    let hex: String = seed.iter().map(|b| format!("{:02x}", b)).collect();
    std::fs::write(&path, hex).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    path
}

async fn client(bare: &tempfile::TempDir, seed: [u8; 32], name: &str, p2p: bool) -> Client {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("file://{}", bare.path().to_str().unwrap());
    Repository::clone(&url, dir.path()).unwrap();
    let path = dir.path().to_path_buf();
    let sync = SyncManager::new_full_with_pull(
        path.to_str().unwrap(),
        "main".to_string(),
        1,
        Some(name.to_string()),
        Some(format!("{}@x.com", name)),
        Some("origin".to_string()),
        None,
        3600,
    );
    let config_dir = tempfile::tempdir().unwrap();
    let config = Config {
        p2p_enabled: p2p,
        p2p_key_file: Some(write_key(config_dir.path(), seed).to_string_lossy().into_owned()),
        p2p_device_name: Some(name.to_string()),
        user_name: Some(name.to_string()),
        user_email: Some(format!("{}@x.com", name)),
        ..Config::default()
    };
    let channel = channel::start(
        ChannelSettings {
            config: &config,
            config_path: &config_dir.path().join("config.json"),
            repo_path: &path,
            branch: "main",
            crypto: None,
            relays: Some(Relays::Disabled),
        },
        &sync,
    )
    .await;
    Client { _dir: dir, _config_dir: config_dir, path, sync, channel }
}

fn link(a: &Client, b: &Client) {
    let (Some(ca), Some(cb)) = (&a.channel, &b.channel) else { return };
    ca.node().add_known_addr(cb.node().local_addr());
    cb.node().add_known_addr(ca.node().local_addr());
}

async fn wait_for(what: &str, limit: Duration, cond: impl Fn() -> bool) -> Duration {
    let start = Instant::now();
    while start.elapsed() < limit {
        if cond() {
            return start.elapsed();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{} did not happen within {:?}", what, limit);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_on_one_device_reaches_the_other_in_seconds() {
    let bare = remote_with_both_devices();
    let a = client(&bare, SEED_A, "a", true).await;
    let b = client(&bare, SEED_B, "b", true).await;
    link(&a, &b);
    let connected = |c: &Client| c.channel.as_ref().unwrap().node().peers().iter().any(|p| p.online);
    wait_for("the connection", Duration::from_secs(10), || connected(&a) && connected(&b)).await;
    // Past the boot pull (~3 s): from here B only pulls when told to.
    tokio::time::sleep(Duration::from_secs(4)).await;

    a.sync.queue_write("hello.txt", b"from A".to_vec());
    let took = wait_for("the file on B", Duration::from_secs(10), || file_exists(&b.path, "hello.txt")).await;
    // Debounce (1 s) + commit + push on A, notice, fetch + merge on B.
    assert!(took < Duration::from_secs(5), "took {:?}", took);
    assert_eq!(std::fs::read(b.path.join("hello.txt")).unwrap(), b"from A");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_the_channel_the_write_waits_for_the_poll() {
    let bare = remote_with_both_devices();
    let a = client(&bare, SEED_A, "a", true).await;
    let b = client(&bare, SEED_B, "b", false).await;
    assert!(b.channel.is_none(), "p2p_enabled false: no channel");
    tokio::time::sleep(Duration::from_secs(4)).await;

    a.sync.queue_write("hello.txt", b"from A".to_vec());
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert!(!file_exists(&b.path, "hello.txt"), "B polls hourly: nothing yet");
    let head_a = Repository::open(&a.path).unwrap().head().unwrap().target().unwrap();
    let remote = Repository::open(bare.path()).unwrap();
    assert_eq!(remote.refname_to_id("refs/heads/main").unwrap(), head_a, "A did push");
}
