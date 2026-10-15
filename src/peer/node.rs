//! Network side of the channel (feature `p2p`): an iroh endpoint that keeps
//! one connection to every registered device, sends `Pushed` after our
//! pushes and wakes our pull watcher when a peer names a commit we lack.
//!
//! Mesh rule: both devices dial each other; when both connections land, the
//! one initiated by the smaller id stays (both sides apply the same rule, so
//! they keep the same one). A connection from an id outside the registry is
//! closed on arrival — and wakes our pull, because that device may have just
//! registered (two devices joining at once only know each other after a
//! pull). Nothing here writes to the repository, and no failure here stops
//! the drive: it only leaves the channel offline, and polling carries on.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::endpoint::{presets, Connection, ConnectionError, VarInt};
use iroh::{Endpoint, EndpointAddr, PublicKey, RelayMode, SecretKey, Watcher};
use parking_lot::Mutex;
use tokio::sync::Notify;

use super::protocol::{should_wake, Message, ALPN, MAX_MESSAGE_BYTES};

/// Where the endpoint finds relays (and the address lookup that goes with them).
#[derive(Debug, Clone)]
pub enum Relays {
    /// Public n0 relays and DNS address lookup.
    N0,
    /// Own relay server (`p2p_relay_url`).
    Custom(String),
    /// No relay, no lookup: direct addresses only (tests).
    Disabled,
}

pub struct NodeConfig {
    pub secret: [u8; 32],
    pub repo_path: PathBuf,
    pub branch: String,
    pub relays: Relays,
}

/// A registered device as the channel sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerState {
    pub id: String,
    pub online: bool,
    /// Unix seconds of the last connection or message; `None` = never.
    pub last_seen: Option<u64>,
}

/// Registry form of a public key: 64 lowercase hex characters.
pub fn hex_id(key: &PublicKey) -> String {
    key.as_bytes().iter().map(|b| format!("{:02x}", b)).collect()
}

fn parse_id(hex: &str) -> Option<PublicKey> {
    if !super::registry::is_device_id(hex) {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    PublicKey::from_bytes(&bytes).ok()
}

/// The device id of a secret, as written in the registry.
pub fn device_id(secret: &[u8; 32]) -> String {
    hex_id(&SecretKey::from_bytes(secret).public())
}

fn now_unix() -> u64 {
    crate::status::now_unix()
}

#[derive(Clone)]
pub struct PeerNode {
    inner: Arc<Inner>,
}

struct Inner {
    endpoint: Endpoint,
    runtime: tokio::runtime::Handle,
    branch: String,
    repo_path: PathBuf,
    pull_now: Arc<Notify>,
    online: Arc<AtomicBool>,
    /// Registered devices other than this one.
    allowed: Mutex<HashSet<PublicKey>>,
    /// Live connection per device, with its generation (a closing connection
    /// never removes its replacement) and the id that initiated it.
    conns: Mutex<HashMap<PublicKey, Live>>,
    last_seen: Mutex<HashMap<PublicKey, u64>>,
    /// Direct addresses known out of band (tests); otherwise the id alone
    /// is dialed and resolved through the address lookup.
    known_addrs: Mutex<HashMap<PublicKey, EndpointAddr>>,
    /// Devices with a dial loop running.
    dialers: Mutex<HashSet<PublicKey>>,
    next_gen: AtomicU64,
    refusal_wakes: Mutex<RefusalWakes>,
    /// Connections refused since start (diagnostics and tests).
    refusals: AtomicU64,
    started: Instant,
}

struct Live {
    generation: u64,
    initiator: PublicKey,
    conn: Connection,
}

/// Close code of a refused connection ("not registered"): the dialer backs
/// off instead of redialing as after a normal drop.
const REFUSED: u32 = 1;

/// When a refused connection may wake our pull (the device may have
/// registered after our last pull). Per device the first refusal wakes at
/// once and the window then doubles, 30 s up to 1 h: a device that
/// registered late is found within a wake or two, a removed one still
/// running costs one fetch an hour. Across devices, at most one wake per
/// 30 s (a stranger can mint ids). Times are seconds on the node's clock.
#[derive(Default)]
struct RefusalWakes {
    last_any: Option<u64>,
    /// Device -> (next allowed wake, current window).
    per_device: HashMap<PublicKey, (u64, u64)>,
}

impl RefusalWakes {
    const SPACING: u64 = 30;
    const FIRST_WINDOW: u64 = 30;
    const MAX_WINDOW: u64 = 3600;

    fn should_wake(&mut self, device: PublicKey, now: u64) -> bool {
        if self.last_any.is_some_and(|at| now < at + Self::SPACING) {
            return false;
        }
        let window = match self.per_device.get(&device) {
            Some(&(next, _)) if now < next => return false,
            Some(&(_, window)) => (window * 2).min(Self::MAX_WINDOW),
            None => Self::FIRST_WINDOW,
        };
        self.per_device.insert(device, (now + window, window));
        self.last_any = Some(now);
        true
    }
}

impl PeerNode {
    /// Binds the endpoint and starts accepting. Dialing starts with
    /// `set_registry`.
    pub async fn start(
        cfg: NodeConfig,
        pull_now: Arc<Notify>,
        online: Arc<AtomicBool>,
    ) -> anyhow::Result<PeerNode> {
        let secret = SecretKey::from_bytes(&cfg.secret);
        let builder = match &cfg.relays {
            Relays::N0 => Endpoint::builder(presets::N0),
            Relays::Custom(url) => {
                let url: iroh::RelayUrl = url.parse()?;
                Endpoint::builder(presets::N0).relay_mode(RelayMode::custom([url]))
            }
            Relays::Disabled => Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind_addr("127.0.0.1:0")?,
        };
        let endpoint = builder
            .secret_key(secret)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;
        let inner = Arc::new(Inner {
            endpoint,
            runtime: tokio::runtime::Handle::current(),
            branch: cfg.branch,
            repo_path: cfg.repo_path,
            pull_now,
            online,
            allowed: Mutex::new(HashSet::new()),
            conns: Mutex::new(HashMap::new()),
            last_seen: Mutex::new(HashMap::new()),
            known_addrs: Mutex::new(HashMap::new()),
            dialers: Mutex::new(HashSet::new()),
            next_gen: AtomicU64::new(0),
            refusal_wakes: Mutex::new(RefusalWakes::default()),
            refusals: AtomicU64::new(0),
            started: Instant::now(),
        });
        log::info!("p2p: endpoint up, device id {}", inner.endpoint.id().fmt_short());

        let accept = inner.clone();
        inner.runtime.spawn(async move { accept.accept_loop().await });
        let watch = inner.clone();
        inner.runtime.spawn(async move {
            while !watch.endpoint.is_closed() {
                watch.refresh_online();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
        Ok(PeerNode { inner })
    }

    /// This device's id, as written in the registry.
    pub fn id(&self) -> String {
        hex_id(&self.inner.endpoint.id())
    }

    /// Direct loopback address of this node (endpoints bound with
    /// `Relays::Disabled`), for peers that cannot look it up.
    pub fn local_addr(&self) -> EndpointAddr {
        let mut addr = EndpointAddr::new(self.inner.endpoint.id());
        for socket in self.inner.endpoint.bound_sockets() {
            if socket.is_ipv4() {
                addr = addr.with_ip_addr(socket);
            }
        }
        addr
    }

    /// Teaches the node a peer's direct address (used instead of the lookup).
    pub fn add_known_addr(&self, addr: EndpointAddr) {
        self.inner.known_addrs.lock().insert(addr.id, addr);
    }

    /// Replaces the set of registered devices (ids in registry form; our own
    /// id is skipped): drops connections to removed devices and starts a
    /// dial loop for every new device this node must dial.
    pub fn set_registry(&self, ids: &[String]) {
        let me = self.inner.endpoint.id();
        let registered = ids.iter().filter_map(|h| parse_id(h)).any(|k| k == me);
        if !registered {
            // Removed with `peers remove` (until its next boot re-registers
            // it): the others refuse it, so it stops knocking.
            log::warn!("p2p: this device is not in the registry — not dialing anyone");
        }
        let wanted: HashSet<PublicKey> = ids
            .iter()
            .filter_map(|h| parse_id(h))
            .filter(|k| *k != me && registered)
            .collect();
        *self.inner.allowed.lock() = wanted.clone();
        let dropped: Vec<Connection> = {
            let mut conns = self.inner.conns.lock();
            let gone: Vec<PublicKey> = conns.keys().filter(|k| !wanted.contains(*k)).copied().collect();
            gone.iter().filter_map(|k| conns.remove(k)).map(|live| live.conn).collect()
        };
        for conn in dropped {
            conn.close(0u32.into(), b"removed from the registry");
        }
        for peer in wanted {
            if self.inner.dialers.lock().insert(peer) {
                let inner = self.inner.clone();
                self.inner.runtime.spawn(async move { inner.dial_loop(peer).await });
            }
        }
    }

    /// Tells every connected device we pushed `commit`. Never blocks the
    /// caller (the drain): sends run on the node's runtime.
    pub fn announce(&self, commit: git2::Oid) {
        let msg = Message::Pushed { branch: self.inner.branch.clone(), commit: commit.to_string() };
        let conns: Vec<Connection> = self.inner.conns.lock().values().map(|l| l.conn.clone()).collect();
        for conn in conns {
            let bytes = msg.encode();
            self.inner.runtime.spawn(async move {
                if let Err(e) = send_bytes(&conn, &bytes).await {
                    log::debug!("p2p: notice to {} failed: {}", conn.remote_id().fmt_short(), e);
                }
            });
        }
    }

    /// Connections refused since start.
    pub fn refusals(&self) -> u64 {
        self.inner.refusals.load(Ordering::SeqCst)
    }

    /// Registered devices with their connection state.
    pub fn peers(&self) -> Vec<PeerState> {
        let allowed: Vec<PublicKey> = self.inner.allowed.lock().iter().copied().collect();
        let conns = self.inner.conns.lock();
        let seen = self.inner.last_seen.lock();
        let mut out: Vec<PeerState> = allowed
            .iter()
            .map(|k| PeerState {
                id: hex_id(k),
                online: conns.contains_key(k),
                last_seen: seen.get(k).copied(),
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub async fn shutdown(&self) {
        self.inner.endpoint.close().await;
        self.inner.online.store(false, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn drop_connections(&self) {
        let conns: Vec<Connection> =
            self.inner.conns.lock().drain().map(|(_, live)| live.conn).collect();
        for conn in conns {
            conn.close(0u32.into(), b"test");
        }
    }
}

async fn send_bytes(conn: &Connection, bytes: &[u8]) -> anyhow::Result<()> {
    let mut send = conn.open_uni().await?;
    send.write_all(bytes).await?;
    send.finish()?;
    Ok(())
}

impl Inner {
    async fn accept_loop(self: Arc<Self>) {
        while let Some(incoming) = self.endpoint.accept().await {
            let inner = self.clone();
            self.runtime.spawn(async move {
                let conn = match incoming.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        log::debug!("p2p: incoming connection failed: {}", e);
                        return;
                    }
                };
                let remote = conn.remote_id();
                if !inner.allowed.lock().contains(&remote) {
                    conn.close(REFUSED.into(), b"not registered");
                    inner.refusals.fetch_add(1, Ordering::SeqCst);
                    if inner.wake_after_refusal(remote) {
                        log::info!(
                            "p2p: refused device {} (not in the registry) — pulling in case it just registered",
                            remote.fmt_short()
                        );
                    } else {
                        log::debug!("p2p: refused device {} (not in the registry)", remote.fmt_short());
                    }
                    return;
                }
                inner.serve(remote, conn, remote).await;
            });
        }
    }

    async fn dial_loop(self: Arc<Self>, peer: PublicKey) {
        let mut backoff = 1u64;
        while !self.endpoint.is_closed() && self.allowed.lock().contains(&peer) {
            if self.conns.lock().contains_key(&peer) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let addr = self
                .known_addrs
                .lock()
                .get(&peer)
                .cloned()
                .unwrap_or_else(|| EndpointAddr::new(peer));
            match tokio::time::timeout(Duration::from_secs(30), self.endpoint.connect(addr, ALPN)).await {
                Ok(Ok(conn)) => {
                    let me = self.endpoint.id();
                    self.clone().serve(peer, conn.clone(), me).await;
                    let refused = matches!(
                        conn.close_reason(),
                        Some(ConnectionError::ApplicationClosed(close))
                            if close.error_code == VarInt::from_u32(REFUSED)
                    );
                    if refused {
                        log::debug!("p2p: {} does not know this device yet", peer.fmt_short());
                    } else {
                        backoff = 1;
                    }
                }
                Ok(Err(e)) => log::debug!("p2p: dialing {} failed: {}", peer.fmt_short(), e),
                Err(_) => log::debug!("p2p: dialing {} timed out", peer.fmt_short()),
            }
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(60);
        }
        self.dialers.lock().remove(&peer);
    }

    /// Runs one connection until it closes: registers it, greets the peer
    /// with our HEAD, then handles every message it sends.
    async fn serve(self: Arc<Self>, remote: PublicKey, conn: Connection, initiator: PublicKey) {
        let generation = self.next_gen.fetch_add(1, Ordering::SeqCst);
        {
            let mut conns = self.conns.lock();
            // Both sides dial: keep the connection the smaller id initiated
            // (a newer one from the same initiator replaces a dying one).
            if let Some(live) = conns.get(&remote) {
                if live.initiator.as_bytes() < initiator.as_bytes() {
                    drop(conns);
                    conn.close(0u32.into(), b"duplicate");
                    return;
                }
            }
            let new = Live { generation, initiator, conn: conn.clone() };
            if let Some(old) = conns.insert(remote, new) {
                old.conn.close(0u32.into(), b"replaced");
            }
        }
        self.touch(remote);
        self.refresh_online();

        if let Some(head) = self.head() {
            let hello = Message::Hello { branch: self.branch.clone(), head: head.to_string() };
            if let Err(e) = send_bytes(&conn, &hello.encode()).await {
                log::debug!("p2p: hello to {} failed: {}", remote.fmt_short(), e);
            }
        }
        // Announced once the peer talks: a peer that refuses us never does,
        // so a refused handshake does not read as "connected".
        let mut talked = false;
        while let Ok(mut recv) = conn.accept_uni().await {
            match recv.read_to_end(MAX_MESSAGE_BYTES).await {
                Ok(bytes) => {
                    if !talked {
                        talked = true;
                        log::info!("p2p: connected to {}", remote.fmt_short());
                    }
                    self.handle(remote, &bytes)
                }
                Err(e) => log::debug!("p2p: unreadable message from {}: {}", remote.fmt_short(), e),
            }
        }

        let removed = {
            let mut conns = self.conns.lock();
            let ours = conns.get(&remote).is_some_and(|live| live.generation == generation);
            if ours {
                conns.remove(&remote);
            }
            ours
        };
        self.refresh_online();
        if removed && talked {
            log::info!("p2p: disconnected from {}", remote.fmt_short());
        } else if removed {
            log::debug!("p2p: connection to {} closed before it talked", remote.fmt_short());
        } else {
            // Replaced by the connection the smaller id initiated.
            log::debug!("p2p: duplicate connection to {} closed", remote.fmt_short());
        }
    }

    fn handle(&self, remote: PublicKey, bytes: &[u8]) {
        self.touch(remote);
        let msg = match Message::decode(bytes) {
            Ok(msg) => msg,
            Err(e) => {
                log::warn!("p2p: dropped a message from {}: {}", remote.fmt_short(), e);
                return;
            }
        };
        // A repo we cannot open counts as "commit unknown": waking the pull
        // costs one fetch, missing a change costs the idle interval.
        let known = |oid: git2::Oid| {
            git2::Repository::open(&self.repo_path)
                .ok()
                .and_then(|r| r.odb().ok().map(|odb| odb.exists(oid)))
                .unwrap_or(false)
        };
        if should_wake(&msg, &self.branch, known) {
            log::info!(
                "p2p: {} has {} — waking the pull",
                remote.fmt_short(),
                &msg.commit()[..8]
            );
            self.pull_now.notify_one();
        }
    }

    fn head(&self) -> Option<git2::Oid> {
        git2::Repository::open(&self.repo_path).ok()?.head().ok()?.target()
    }

    /// A refused device may have registered after our last pull: look,
    /// within the `RefusalWakes` budget. Returns whether the pull was woken.
    fn wake_after_refusal(&self, device: PublicKey) -> bool {
        let now = self.started.elapsed().as_secs();
        let wake = self.refusal_wakes.lock().should_wake(device, now);
        if wake {
            self.pull_now.notify_one();
        }
        wake
    }

    fn touch(&self, remote: PublicKey) {
        self.last_seen.lock().insert(remote, now_unix());
    }

    /// Online = a relay connection or at least one live peer connection.
    fn refresh_online(&self) {
        let relay = self.endpoint.home_relay_status().get().iter().any(|s| s.is_connected());
        let connected: Vec<PublicKey> = self.conns.lock().keys().copied().collect();
        for peer in &connected {
            self.touch(*peer);
        }
        self.online.store(relay || !connected.is_empty(), Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};

    fn repo_with_commit(content: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();
        std::fs::write(dir.path().join("f.txt"), content).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("f.txt")).unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("T", "t@x.com").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[]).unwrap();
        dir
    }

    struct TestNode {
        node: PeerNode,
        pull_now: Arc<Notify>,
        online: Arc<AtomicBool>,
    }

    async fn start(seed: u8, repo: &std::path::Path) -> TestNode {
        let pull_now = Arc::new(Notify::new());
        let online = Arc::new(AtomicBool::new(false));
        let cfg = NodeConfig {
            secret: [seed; 32],
            repo_path: repo.to_path_buf(),
            branch: crate::git::current_branch_name(&Repository::open(repo).unwrap()).unwrap(),
            relays: Relays::Disabled,
        };
        let node = PeerNode::start(cfg, pull_now.clone(), online.clone()).await.unwrap();
        TestNode { node, pull_now, online }
    }

    fn link(a: &TestNode, b: &TestNode) {
        a.node.add_known_addr(b.node.local_addr());
        b.node.add_known_addr(a.node.local_addr());
        let ids = vec![a.node.id(), b.node.id()];
        a.node.set_registry(&ids);
        b.node.set_registry(&ids);
    }

    async fn wait_until(what: &str, cond: impl Fn() -> bool) {
        for _ in 0..100 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {}", what);
    }

    async fn woken(n: &TestNode, within: Duration) -> bool {
        tokio::time::timeout(within, n.pull_now.notified()).await.is_ok()
    }

    fn connected(n: &TestNode) -> bool {
        n.node.peers().iter().any(|p| p.online)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pushed_notice_wakes_the_peer_pull() {
        let repo = repo_with_commit("same");
        let a = start(1, repo.path()).await;
        let b = start(2, repo.path()).await;
        link(&a, &b);
        wait_until("connection", || connected(&a) && connected(&b)).await;
        assert!(!woken(&b, Duration::from_millis(300)).await, "shared HEAD: hello wakes nobody");

        a.node.announce(git2::Oid::from_str(&"ab".repeat(20)).unwrap());
        assert!(woken(&b, Duration::from_secs(5)).await, "unknown commit wakes the pull");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hello_with_an_unknown_head_wakes_on_connect() {
        let repo_a = repo_with_commit("A's history");
        let repo_b = repo_with_commit("B's history");
        let a = start(1, repo_a.path()).await;
        let b = start(2, repo_b.path()).await;
        link(&a, &b);
        assert!(woken(&b, Duration::from_secs(5)).await, "B learns A is ahead on connect");
        assert!(woken(&a, Duration::from_secs(5)).await, "and A learns about B");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_outside_the_registry_is_refused() {
        let repo = repo_with_commit("x");
        let a = start(1, repo.path()).await;
        let b = start(2, repo.path()).await;
        a.node.set_registry(&[a.node.id(), b.node.id()]);
        // An endpoint whose id is not registered dials A directly.
        let stranger = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind_addr("127.0.0.1:0")
            .unwrap()
            .secret_key(SecretKey::from_bytes(&[9; 32]))
            .bind()
            .await
            .unwrap();
        let conn = stranger.connect(a.node.local_addr(), ALPN).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), conn.closed())
            .await
            .expect("A closes the connection");
        assert!(!a.node.peers().iter().any(|p| p.online), "the stranger never counts as a peer");
        // It may be a device that just registered: one pull to find out...
        assert!(woken(&a, Duration::from_secs(2)).await);
        // ...but a stranger dialing again cannot turn refusals into fetches.
        let again = stranger.connect(a.node.local_addr(), ALPN).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), again.closed()).await.unwrap();
        assert!(!woken(&a, Duration::from_millis(500)).await, "rate limited");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_late_registration_is_accepted_once_the_registry_catches_up() {
        // Two devices registered at the same time: B already knows A, A does
        // not know B yet. B's dial is refused, and the refusal wakes A's pull
        // (B may have just registered); after A reloads the registry, the
        // connection forms — whatever the order of the ids.
        let repo = repo_with_commit("x");
        for (seed_a, seed_b) in [(1u8, 2u8), (2, 1)] {
            let a = start(seed_a, repo.path()).await;
            let b = start(seed_b, repo.path()).await;
            a.node.add_known_addr(b.node.local_addr());
            b.node.add_known_addr(a.node.local_addr());
            a.node.set_registry(&[a.node.id()]);
            b.node.set_registry(&[a.node.id(), b.node.id()]);
            assert!(woken(&a, Duration::from_secs(5)).await, "refusal wakes A's pull");
            a.node.set_registry(&[a.node.id(), b.node.id()]);
            wait_until("late connection", || connected(&a) && connected(&b)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn both_sides_dialing_settle_on_one_connection() {
        let repo = repo_with_commit("x");
        let a = start(1, repo.path()).await;
        let b = start(2, repo.path()).await;
        link(&a, &b);
        wait_until("connection", || connected(&a) && connected(&b)).await;
        // No flapping: the pair stays connected and notices keep flowing.
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(connected(&a) && connected(&b));
        }
        b.node.announce(git2::Oid::from_str(&"ef".repeat(20)).unwrap());
        assert!(woken(&a, Duration::from_secs(5)).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_refused_dialer_backs_off_instead_of_redialing_every_second() {
        // B does not know A (A registered after B's last pull, or A was
        // removed): A must back off (1, 2, 4 s...) instead of treating every
        // accepted-then-refused handshake as a success (production bootstrap,
        // 14-10-2026: 33 refusals in 70 s).
        let repo = repo_with_commit("x");
        let a = start(1, repo.path()).await;
        let b = start(2, repo.path()).await;
        a.node.add_known_addr(b.node.local_addr());
        a.node.set_registry(&[a.node.id(), b.node.id()]);
        b.node.set_registry(&[b.node.id()]);
        tokio::time::sleep(Duration::from_secs(6)).await;
        let refused = b.node.refusals();
        assert!((1..=3).contains(&refused), "{} refusals in 6 s", refused);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_device_missing_from_its_own_registry_dials_nobody() {
        // Removed with `peers remove`: until it re-registers (next boot), it
        // must not keep knocking on the others.
        let repo = repo_with_commit("x");
        let a = start(1, repo.path()).await;
        let b = start(2, repo.path()).await;
        a.node.add_known_addr(b.node.local_addr());
        a.node.set_registry(&[b.node.id()]);
        b.node.set_registry(&[b.node.id()]);
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(b.node.refusals(), 0, "A never dialed");
    }

    #[test]
    fn refusal_wakes_back_off_per_device() {
        let mut schedule = RefusalWakes::default();
        let (a, b) = (SecretKey::from_bytes(&[1; 32]).public(), SecretKey::from_bytes(&[2; 32]).public());
        assert!(schedule.should_wake(a, 0), "first refusal of a device wakes");
        assert!(!schedule.should_wake(b, 10), "global spacing: 30 s between any two wakes");
        assert!(schedule.should_wake(b, 40), "another device after the global spacing");
        assert!(!schedule.should_wake(a, 40), "a's next wake is 30 s after its first... and global");
        assert!(schedule.should_wake(a, 75), "a: 30 s window passed");
        assert!(!schedule.should_wake(a, 120), "a's window doubled to 60 s");
        assert!(schedule.should_wake(a, 140));
        let mut late = 140;
        for _ in 0..12 {
            late += 4000;
            schedule.should_wake(a, late);
        }
        assert!(!schedule.should_wake(a, late + 3000), "capped at one wake per hour");
        assert!(schedule.should_wake(a, late + 3700));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn online_follows_the_connections() {
        let repo = repo_with_commit("x");
        let a = start(1, repo.path()).await;
        let b = start(2, repo.path()).await;
        assert!(!a.online.load(Ordering::SeqCst), "no relay, no peer: offline");
        link(&a, &b);
        wait_until("A online", || a.online.load(Ordering::SeqCst)).await;
        b.node.shutdown().await;
        wait_until("A offline", || !a.online.load(Ordering::SeqCst)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_dialer_reconnects_after_the_connection_drops() {
        let repo = repo_with_commit("x");
        let a = start(1, repo.path()).await;
        let b = start(2, repo.path()).await;
        link(&a, &b);
        wait_until("first connection", || connected(&a) && connected(&b)).await;
        b.node.drop_connections();
        wait_until("drop seen", || !connected(&a) || !connected(&b)).await;
        wait_until("reconnection", || connected(&a) && connected(&b)).await;
        a.node.announce(git2::Oid::from_str(&"cd".repeat(20)).unwrap());
        assert!(woken(&b, Duration::from_secs(5)).await, "notices flow again");
    }
}
