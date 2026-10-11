//! `Node`: the running Lancat core. Owns the store, peer table, links and transfers.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use tokio::sync::{broadcast, mpsc, watch};

use crate::events::{CoreEvent, PeerView};
use crate::files::IncomingTransfer;
use crate::netutil;
use crate::protocol::*;
use crate::store::{FileBody, Message, Settings, Store};

/// Startup configuration (not user settings; those live in the store).
#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub data_dir: PathBuf,
    /// TCP port for HTTP/WebSocket. 0 = pick a free port (tests).
    pub port: u16,
    /// UDP discovery port (probes / replies / bye).
    pub udp_port: u16,
    pub enable_mdns: bool,
    pub enable_udp: bool,
    /// Overrides the display name (e.g. LANCAT_NAME for a second local instance).
    pub name_override: Option<String>,
    /// Overrides the receive directory (LANCAT_RECV_DIR).
    pub recv_dir_override: Option<PathBuf>,
    /// Extra unicast probe targets (ip:udp_port), e.g. LANCAT_PROBE=127.0.0.1:47821 for tests.
    pub extra_probe_targets: Vec<SocketAddr>,
}

impl NodeConfig {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
            port: DEFAULT_PORT,
            udp_port: DEFAULT_PORT,
            enable_mdns: true,
            enable_udp: true,
            name_override: None,
            recv_dir_override: None,
            extra_probe_targets: vec![],
        }
    }

    /// Applies LANCAT_* environment overrides, so two instances can run on one machine:
    /// `LANCAT_PORT=47821 LANCAT_DATA_DIR=/tmp/lc2 LANCAT_NAME=second lancat`
    pub fn with_env(mut self) -> Self {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        if let Some(d) = env("LANCAT_DATA_DIR") {
            self.data_dir = d.into();
        }
        if let Some(p) = env("LANCAT_PORT").and_then(|p| p.parse().ok()) {
            self.port = p;
        }
        if let Some(p) = env("LANCAT_UDP_PORT").and_then(|p| p.parse().ok()) {
            self.udp_port = p;
        }
        if env("LANCAT_NO_MDNS").is_some() {
            self.enable_mdns = false;
        }
        if env("LANCAT_NO_UDP").is_some() {
            self.enable_udp = false;
        }
        if let Some(n) = env("LANCAT_NAME") {
            self.name_override = Some(n);
        }
        if let Some(r) = env("LANCAT_RECV_DIR") {
            self.recv_dir_override = Some(r.into());
        }
        if let Some(p) = env("LANCAT_PROBE") {
            self.extra_probe_targets.extend(p.split(',').filter_map(|a| a.trim().parse::<SocketAddr>().ok()));
        }
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    pub info: DeviceInfo,
    pub addr: IpAddr,
    pub last_seen: Instant,
    pub online: bool,
    /// The last few addresses the peer was heard on (primary included), newest last. A peer that
    /// can dial us is not always reachable on the address it dials from (its firewall drops
    /// inbound connections on that network, e.g. a Windows "Public" Wi-Fi), so outgoing HTTP
    /// (file transfers) falls back to another one of these (Tailscale ...). Candidates only:
    /// every one is checked with `GET /info` before use.
    pub seen_at: Vec<(IpAddr, Instant)>,
}

const SEEN_ADDR_MAX: usize = 6;

impl PeerState {
    fn note_addr(&mut self, ip: IpAddr) {
        self.seen_at.retain(|(a, _)| *a != ip);
        self.seen_at.push((ip, Instant::now()));
        if self.seen_at.len() > SEEN_ADDR_MAX {
            let extra = self.seen_at.len() - SEEN_ADDR_MAX;
            self.seen_at.drain(..extra);
        }
    }
}

pub(crate) struct Link {
    pub tx: mpsc::UnboundedSender<String>,
    pub conn_id: u64,
    /// Dialed by the device with the smaller id (wins over a simultaneous second connection).
    pub preferred: bool,
}

pub struct Node {
    pub cfg: NodeConfig,
    pub store: Store,
    me: RwLock<DeviceInfo>,
    events: broadcast::Sender<CoreEvent>,
    pub(crate) peers: RwLock<HashMap<String, PeerState>>,
    pub(crate) links: Mutex<HashMap<String, Link>>,
    pub(crate) dialing: Mutex<HashSet<String>>,
    /// `peer@ip` -> last attempt to move a link onto that LAN address
    pub(crate) upgrade_tries: Mutex<HashMap<String, Instant>>,
    pub(crate) incoming: Mutex<HashMap<String, Arc<IncomingTransfer>>>,
    pub(crate) active_out: Mutex<HashSet<String>>,
    /// Pause / cancel switches of the outgoing transfers that are running right now.
    pub(crate) out_ctl: Mutex<HashMap<String, Arc<crate::transfer::TransferCtl>>>,
    /// Outgoing transfers allowed to move data at once (see transfer::MAX_ACTIVE_TRANSFERS).
    pub(crate) transfer_slots: tokio::sync::Semaphore,
    /// Incoming chunk bodies buffered / written at once (see server::file_chunk).
    pub(crate) chunk_slots: tokio::sync::Semaphore,
    pub(crate) http: reqwest::Client,
    pub(crate) conn_seq: AtomicU64,
    pub(crate) mdns: Mutex<Option<crate::discovery::Mdns>>,
    /// Kicks a discovery probe burst (start, network change, wake, manual refresh).
    pub(crate) probe_kick: tokio::sync::Notify,
    /// Wakes housekeeping early (a peer came online without a link, a link went down).
    hk_kick: tokio::sync::Notify,
    /// Peers whose queue is being flushed right now -> "flush again when done" flag.
    flushing: Mutex<HashMap<String, bool>>,
    pub(crate) udp: OnceLock<Arc<tokio::net::UdpSocket>>,
    stopped: AtomicBool,
    shutdown_tx: watch::Sender<bool>,
    pub(crate) rt: tokio::runtime::Handle,
}

impl Node {
    /// Starts the node: opens the store, binds TCP, starts discovery and housekeeping.
    pub async fn start(cfg: NodeConfig) -> Result<Arc<Node>> {
        std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("create {}", cfg.data_dir.display()))?;
        let store = Store::open(&cfg.data_dir.join("lancat.db"))?;
        store.mark_interrupted().ok();

        let id = match store.kv_get("device_id")? {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                store.kv_set("device_id", &id)?;
                id
            }
        };
        let settings = store.settings();
        let name = cfg
            .name_override
            .clone()
            .or_else(|| Some(settings.device_name.clone()).filter(|n| !n.is_empty()))
            .unwrap_or_else(netutil::host_name);

        let listener = tokio::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, cfg.port)))
            .await
            .with_context(|| format!("bind TCP port {}", cfg.port))?;
        let port = listener.local_addr()?.port();

        let me = DeviceInfo { id, name, os: netutil::os_name().into(), ver: env!("CARGO_PKG_VERSION").into(), port };
        let (events, _) = broadcast::channel(1024);
        let (shutdown_tx, _) = watch::channel(false);
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(4))
            .pool_idle_timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()?;

        let node = Arc::new(Node {
            cfg,
            store,
            me: RwLock::new(me),
            events,
            peers: RwLock::new(HashMap::new()),
            links: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashSet::new()),
            upgrade_tries: Mutex::new(HashMap::new()),
            incoming: Mutex::new(HashMap::new()),
            active_out: Mutex::new(HashSet::new()),
            out_ctl: Mutex::new(HashMap::new()),
            transfer_slots: tokio::sync::Semaphore::new(crate::transfer::MAX_ACTIVE_TRANSFERS),
            chunk_slots: tokio::sync::Semaphore::new(crate::server::MAX_INCOMING_CHUNKS),
            http,
            conn_seq: AtomicU64::new(1),
            mdns: Mutex::new(None),
            probe_kick: tokio::sync::Notify::new(),
            hk_kick: tokio::sync::Notify::new(),
            flushing: Mutex::new(HashMap::new()),
            udp: OnceLock::new(),
            stopped: AtomicBool::new(false),
            shutdown_tx,
            rt: tokio::runtime::Handle::current(),
        });

        crate::server::spawn(node.clone(), listener);
        crate::discovery::start(node.clone());
        node.clone().spawn_housekeeping();
        log::info!("lancat node {} ({}) listening on :{}", node.me().id, node.me().name, port);
        Ok(node)
    }

    pub fn me(&self) -> DeviceInfo {
        self.me.read().unwrap().clone()
    }

    pub fn port(&self) -> u16 {
        self.me.read().unwrap().port
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CoreEvent> {
        self.events.subscribe()
    }

    pub(crate) fn emit(&self, ev: CoreEvent) {
        let _ = self.events.send(ev);
    }

    pub(crate) fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.shutdown_tx.subscribe()
    }

    /// Clean exit: tells peers we are leaving (UDP `bye` + `bye` over each link), then stops.
    /// Idempotent.
    pub fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        crate::discovery::goodbye(self);
        let bye = serde_json::to_string(&WsMsg::Bye).unwrap_or_default();
        for l in self.links.lock().unwrap().values() {
            let _ = l.tx.send(bye.clone());
        }
        let _ = self.shutdown_tx.send(true);
        crate::discovery::stop(self);
        // dropping the senders lets each link writer flush the bye and close the socket
        self.links.lock().unwrap().clear();
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Manual refresh: one probe burst on every interface (UI refresh button / tray item).
    pub fn refresh_peers(&self) {
        self.kick_probe("manual refresh");
    }

    pub(crate) fn kick_probe(&self, reason: &str) {
        if self.is_stopped() {
            return;
        }
        log::info!("discovery: probe burst ({reason})");
        self.probe_kick.notify_one();
    }

    pub fn online_count(&self) -> usize {
        self.peers.read().unwrap().values().filter(|p| p.online).count()
    }

    // ------------------------------------------------------------------ settings
    pub fn settings(&self) -> Settings {
        self.store.settings()
    }

    pub fn set_settings(&self, s: Settings) -> Result<Settings> {
        let old = self.store.settings();
        self.store.save_settings(&s)?;
        if s.device_name != old.device_name && self.cfg.name_override.is_none() {
            let name = if s.device_name.trim().is_empty() { netutil::host_name() } else { s.device_name.trim().to_string() };
            self.me.write().unwrap().name = name;
            crate::discovery::reannounce(self);
            // let connected peers know
            let hello = WsMsg::Hello { device: self.me() };
            let txt = serde_json::to_string(&hello).unwrap_or_default();
            for l in self.links.lock().unwrap().values() {
                let _ = l.tx.send(txt.clone());
            }
        }
        self.emit(CoreEvent::Settings { settings: s.clone() });
        Ok(s)
    }

    pub fn receive_dir(&self) -> PathBuf {
        if let Some(d) = &self.cfg.recv_dir_override {
            return d.clone();
        }
        let s = self.store.settings();
        if !s.receive_dir.trim().is_empty() {
            return PathBuf::from(s.receive_dir.trim());
        }
        dirs::download_dir().unwrap_or_else(|| self.cfg.data_dir.join("Downloads"))
    }

    // ------------------------------------------------------------------ peers
    /// Called whenever we hear from a peer (mDNS, UDP, WS, HTTP).
    pub(crate) fn peer_seen(self: &Arc<Self>, info: DeviceInfo, addr: IpAddr) {
        if info.id == self.me().id || info.id.is_empty() {
            return;
        }
        let mut upgrade: Option<(IpAddr, IpAddr)> = None;
        let (changed, came_online) = {
            let mut peers = self.peers.write().unwrap();
            let heard = addr;
            match peers.get_mut(&info.id) {
                Some(p) => {
                    // A peer reachable on both the LAN and an overlay (Tailscale ...) answers on
                    // both; keep the better path while it is fresh instead of flapping.
                    // While a link is up its address is proven; a probe that arrived via another
                    // interface (a second subnet, a VM bridge) must not move the peer.
                    let linked = self.links.lock().unwrap().contains_key(&info.id);
                    let fresh = p.online && p.last_seen.elapsed() < Duration::from_secs(120);
                    // ... except that a LAN address of one of our physical networks beats an overlay
                    // (Tailscale) path even while linked: the link is moved over (`upgrade_link`).
                    if fresh && linked && addr_rank(&addr) == 0 && addr_rank(&p.addr) > 0 {
                        upgrade = Some((p.addr, addr));
                    }
                    let addr = if fresh && (linked || addr_rank(&addr) > addr_rank(&p.addr)) { p.addr } else { addr };
                    p.note_addr(heard);
                    let changed = p.info != info || p.addr != addr || !p.online;
                    let came_online = !p.online;
                    p.info = info.clone();
                    p.addr = addr;
                    p.last_seen = Instant::now();
                    p.online = true;
                    (changed, came_online)
                }
                None => {
                    peers.insert(
                        info.id.clone(),
                        PeerState { info: info.clone(), addr, last_seen: Instant::now(), online: true, seen_at: vec![(addr, Instant::now())] },
                    );
                    (true, true)
                }
            }
        };
        let addr = self.peers.read().unwrap().get(&info.id).map(|p| p.addr).unwrap_or(addr);
        if changed {
            let _ = self.store.upsert_device(&info, &addr.to_string(), now_ms());
            self.emit_peer(&info.id);
        }
        if let Some((from, to)) = upgrade {
            self.maybe_upgrade_link(&info.id, from, to, info.port);
        }
        if came_online {
            self.kick_housekeeping();
            log::info!("peer online: {} ({}) at {}:{}", info.name, info.id, addr, info.port);
            let node = self.clone();
            let id = info.id.clone();
            self.rt.spawn(async move {
                if node.me().id > id {
                    // we are not the dialer: introduce ourselves so the peer can dial us
                    node.introduce(&id).await;
                }
                node.ensure_link(&id).await;
                // give a dial a moment, then flush anything queued (HTTP fallback if no link)
                tokio::time::sleep(Duration::from_millis(300)).await;
                node.flush_peer(&id).await;
            });
        }
    }

    /// A linked peer runs over an overlay address (`from`, e.g. Tailscale) but was just heard on a
    /// LAN address (`to`). If `to` is on one of our physical (non-virtual) networks and answers,
    /// dial a new link there; it replaces the old one (both ends keep the newest link). Only the
    /// designated dialer does this, so both ends never race two replacement links.
    fn maybe_upgrade_link(self: &Arc<Self>, id: &str, from: IpAddr, to: IpAddr, port: u16) {
        if self.me().id.as_str() >= id || !on_physical_lan(&to) {
            return;
        }
        {
            let mut tried = self.upgrade_tries.lock().unwrap();
            let key = format!("{id}@{to}");
            if tried.get(&key).is_some_and(|t| t.elapsed() < Duration::from_secs(120)) {
                return;
            }
            tried.insert(key, Instant::now());
        }
        let node = self.clone();
        let id = id.to_string();
        self.rt.spawn(async move {
            let sa = SocketAddr::new(to, port);
            let ok = matches!(tokio::time::timeout(Duration::from_millis(1500), tokio::net::TcpStream::connect(sa)).await, Ok(Ok(_)));
            if !ok {
                log::debug!("{id}: LAN address {to} not reachable, staying on {from}");
                return;
            }
            log::info!("{id}: moving the link from {from} to LAN {to}");
            node.dial_link_at(&id, to).await;
        });
    }

    pub(crate) fn set_peer_addr(&self, id: &str, ip: IpAddr) {
        let info = {
            let mut peers = self.peers.write().unwrap();
            match peers.get_mut(id) {
                Some(p) if p.addr != ip => {
                    p.addr = ip;
                    p.note_addr(ip);
                    Some(p.info.clone())
                }
                _ => None,
            }
        };
        if let Some(info) = info {
            let _ = self.store.upsert_device(&info, &ip.to_string(), now_ms());
            self.emit_peer(id);
        }
    }

    /// The peer said goodbye (clean exit): offline now, drop the link.
    pub(crate) fn peer_bye(&self, id: &str, how: &str) {
        let was = {
            let mut peers = self.peers.write().unwrap();
            match peers.get_mut(id) {
                Some(p) if p.online => {
                    p.online = false;
                    true
                }
                _ => false,
            }
        };
        self.links.lock().unwrap().remove(id);
        if was {
            log::info!("peer offline: {id} ({how})");
            self.emit_peer(id);
        }
    }

    /// A link closed without a goodbye. Ping timeout: the peer has been silent for 60–90 s, it is
    /// offline. Connection closed: re-probe it and give it a few seconds to answer (or re-link)
    /// before housekeeping marks it offline.
    pub(crate) fn link_lost(&self, id: &str, timed_out: bool) {
        if self.is_stopped() {
            return;
        }
        let addr = {
            let mut peers = self.peers.write().unwrap();
            let Some(p) = peers.get_mut(id) else { return };
            if !p.online {
                return;
            }
            if timed_out {
                p.online = false;
                None
            } else {
                let keep = Duration::from_secs(LINKLESS_GRACE_SECS - LINK_LOST_GRACE_SECS);
                p.last_seen = Instant::now().checked_sub(keep).unwrap_or(p.last_seen);
                Some(p.addr)
            }
        };
        match addr {
            None => {
                log::info!("peer offline: {id} (no pong for {} pings)", LINK_MISSED_PINGS);
                self.emit_peer(id);
            }
            Some(ip) => {
                log::info!("link to {id} closed: re-probing {ip}");
                crate::discovery::probe_addr(self, ip);
            }
        }
        self.kick_housekeeping();
    }

    pub(crate) fn kick_housekeeping(&self) {
        self.hk_kick.notify_one();
    }

    /// Refreshes liveness without other changes (pings, chunk traffic).
    pub(crate) fn touch_peer(&self, id: &str) {
        if let Some(p) = self.peers.write().unwrap().get_mut(id) {
            p.last_seen = Instant::now();
        }
    }

    pub(crate) fn peer_addr(&self, id: &str) -> Option<(IpAddr, u16)> {
        if let Some(p) = self.peers.read().unwrap().get(id) {
            return Some((p.addr, p.info.port));
        }
        let d = self.store.device(id)?;
        Some((d.last_addr.parse().ok()?, d.port))
    }

    /// Addresses to dial the peer at, best first: the current address, then the other addresses it
    /// was heard on (newest first; IPv6 link-local skipped, it would need a scope id).
    pub(crate) fn peer_dial_addrs(&self, id: &str) -> Vec<(IpAddr, u16)> {
        let Some((ip, port)) = self.peer_addr(id) else { return vec![] };
        let mut out = vec![(ip, port)];
        if let Some(p) = self.peers.read().unwrap().get(id) {
            for (a, _) in p.seen_at.iter().rev() {
                let ll6 = matches!(a, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80);
                if !ll6 && !out.iter().any(|(o, _)| o == a) {
                    out.push((*a, port));
                }
            }
        }
        out
    }

    pub fn is_online(&self, id: &str) -> bool {
        self.peers.read().unwrap().get(id).map(|p| p.online).unwrap_or(false)
    }

    pub fn peer_view(&self, id: &str) -> Option<PeerView> {
        let row = self.store.device(id)?;
        let peers = self.peers.read().unwrap();
        let rt = peers.get(id);
        Some(PeerView {
            id: row.id.clone(),
            name: rt.map(|p| p.info.name.clone()).unwrap_or(row.name),
            os: row.os,
            ver: row.ver,
            online: rt.map(|p| p.online).unwrap_or(false),
            addr: rt.map(|p| p.addr.to_string()).unwrap_or(row.last_addr),
            port: rt.map(|p| p.info.port).unwrap_or(row.port),
            blocked: row.blocked,
            unread: row.unread,
            last_ts: row.last_ts,
            last_preview: row.last_preview,
        })
    }

    pub fn peers(&self) -> Vec<PeerView> {
        let rows = self.store.devices().unwrap_or_default();
        let peers = self.peers.read().unwrap();
        let mut v: Vec<PeerView> = rows
            .into_iter()
            .map(|row| {
                let rt = peers.get(&row.id);
                PeerView {
                    id: row.id.clone(),
                    name: rt.map(|p| p.info.name.clone()).unwrap_or(row.name),
                    os: row.os,
                    ver: row.ver,
                    online: rt.map(|p| p.online).unwrap_or(false),
                    addr: rt.map(|p| p.addr.to_string()).unwrap_or(row.last_addr),
                    port: rt.map(|p| p.info.port).unwrap_or(row.port),
                    blocked: row.blocked,
                    unread: row.unread,
                    last_ts: row.last_ts,
                    last_preview: row.last_preview,
                }
            })
            .collect();
        // online first, then most recent conversation
        v.sort_by(|a, b| b.online.cmp(&a.online).then(b.last_ts.cmp(&a.last_ts)).then(a.name.cmp(&b.name)));
        v
    }

    pub(crate) fn emit_peer(&self, id: &str) {
        if let Some(p) = self.peer_view(id) {
            self.emit(CoreEvent::Peer { peer: p });
        }
    }

    /// Manually add a peer by "ip:port" (or "ip", default port) — for networks that block multicast/broadcast.
    pub async fn add_peer_addr(self: &Arc<Self>, addr: &str) -> Result<PeerView> {
        let sa: SocketAddr = match addr.parse() {
            Ok(sa) => sa,
            Err(_) => SocketAddr::new(addr.parse::<IpAddr>().map_err(|_| anyhow!("无效地址: {addr}"))?, DEFAULT_PORT),
        };
        if !netutil::is_private(&sa.ip()) {
            bail!("只允许局域网地址");
        }
        let resp = self
            .http
            .get(format!("http://{}/info", fmt_sa(&sa)))
            .header(HEADER_DEVICE_ID, self.me().id)
            .timeout(Duration::from_secs(4))
            .send()
            .await?
            .error_for_status()?;
        let info: DeviceInfo = json_body(resp).await?;
        if info.id == self.me().id {
            bail!("这是本机");
        }
        self.peer_seen(info.clone(), sa.ip());
        {
            let node = self.clone();
            let id = info.id.clone();
            tokio::spawn(async move {
                node.introduce(&id).await;
                node.ensure_link(&id).await;
                node.flush_peer(&id).await;
            });
        }
        self.peer_view(&info.id).ok_or_else(|| anyhow!("peer vanished"))
    }

    pub fn set_blocked(&self, id: &str, blocked: bool) -> Result<()> {
        self.store.set_blocked(id, blocked)?;
        if blocked {
            self.links.lock().unwrap().remove(id);
        }
        self.emit_peer(id);
        Ok(())
    }

    pub fn forget_peer(&self, id: &str) -> Result<()> {
        self.store.forget_device(id)?;
        self.peers.write().unwrap().remove(id);
        self.links.lock().unwrap().remove(id);
        self.emit(CoreEvent::PeerRemoved { peer_id: id.into() });
        Ok(())
    }

    pub(crate) fn is_blocked_addr(&self, ip: &IpAddr) -> bool {
        let s = ip.to_string();
        let st = self.store.settings();
        st.blocked_ips.iter().any(|b| b.trim() == s) || self.store.blocked_addrs().iter().any(|b| *b == s)
    }

    // ------------------------------------------------------------------ housekeeping
    /// Offline detection and reconnects for peers that are online without a link. Event driven:
    /// it used to wake every second for the whole life of the app; now it only ticks (every 3 s,
    /// the reconnect cadence) while such a peer exists and otherwise sleeps until kicked (a peer
    /// came online, a link went down), with a long safety interval.
    fn spawn_housekeeping(self: Arc<Self>) {
        let mut stop = self.shutdown_rx();
        let node = self;
        tokio::spawn(async move {
            const BUSY: Duration = Duration::from_secs(3);
            const IDLE: Duration = Duration::from_secs(60);
            let mut wait = BUSY;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = node.hk_kick.notified() => {}
                    _ = stop.changed() => break,
                }
                let mut gone = vec![];
                let mut need_link = vec![];
                {
                    let mut peers = node.peers.write().unwrap();
                    let links = node.links.lock().unwrap();
                    // Liveness is the link (ping/pong, close, bye). A peer without a link stays
                    // online only while it shows other signs of life (probes, HTTP).
                    for (id, p) in peers.iter_mut() {
                        if !p.online || links.contains_key(id) {
                            continue;
                        }
                        let quiet = p.last_seen.elapsed();
                        if quiet > Duration::from_secs(LINKLESS_GRACE_SECS) {
                            p.online = false;
                            gone.push(id.clone());
                        } else {
                            // the designated dialer retries every 3 s; if it cannot reach us
                            // (firewall on its side of the path), we dial after 6 s instead
                            need_link.push((id.clone(), quiet > Duration::from_secs(6)));
                        }
                    }
                }
                for id in gone {
                    log::info!("peer offline: {id}");
                    node.links.lock().unwrap().remove(&id);
                    node.emit_peer(&id);
                }
                // reconnect (local work only; nothing is sent to peers we have not found)
                wait = if need_link.is_empty() { IDLE } else { BUSY };
                for (id, force) in need_link {
                    let node = node.clone();
                    tokio::spawn(async move { node.dial_link(&id, force).await });
                }
            }
        });
    }

    // ------------------------------------------------------------------ messaging
    fn new_outgoing(&self, peer_id: &str, kind: &str, body: serde_json::Value) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            peer_id: peer_id.into(),
            outgoing: true,
            kind: kind.into(),
            body,
            status: "pending".into(),
            ts: now_ms(),
        }
    }

    pub async fn send_text(self: &Arc<Self>, peer_id: &str, text: &str) -> Result<Message> {
        if text.is_empty() {
            bail!("空消息");
        }
        let m = self.new_outgoing(peer_id, "text", serde_json::json!({ "text": text }));
        self.store.insert_message(&m, false)?;
        self.emit(CoreEvent::Message { message: m.clone() });
        self.deliver(&m).await;
        Ok(self.store.message(&m.id)?.unwrap_or(m))
    }

    pub async fn send_emoji(self: &Arc<Self>, peer_id: &str, emoji: EmojiPayload) -> Result<Message> {
        let m = self.new_outgoing(peer_id, "emoji", serde_json::to_value(&emoji)?);
        self.store.insert_message(&m, false)?;
        self.emit(CoreEvent::Message { message: m.clone() });
        self.deliver(&m).await;
        Ok(self.store.message(&m.id)?.unwrap_or(m))
    }

    pub fn send_typing(&self, peer_id: &str, typing: bool) {
        let msg = serde_json::to_string(&WsMsg::Typing { typing }).unwrap_or_default();
        if let Some(l) = self.links.lock().unwrap().get(peer_id) {
            let _ = l.tx.send(msg);
        }
    }

    fn to_wire(m: &Message) -> Option<WsMsg> {
        match m.kind.as_str() {
            "text" => Some(WsMsg::Text { id: m.id.clone(), text: m.body.get("text")?.as_str()?.to_string(), ts: m.ts }),
            "emoji" => Some(WsMsg::Emoji { id: m.id.clone(), emoji: serde_json::from_value(m.body.clone()).ok()?, ts: m.ts }),
            _ => None,
        }
    }

    /// Delivers a text/emoji message: over the WebSocket link when present, else HTTP POST /msg.
    pub(crate) async fn deliver(self: &Arc<Self>, m: &Message) {
        let Some(wire) = Self::to_wire(m) else { return };
        if self.send_over_link(&m.peer_id, &wire) {
            if m.status == "pending" {
                self.set_status(&m.id, "sent");
            }
            return;
        }
        if !self.is_online(&m.peer_id) {
            return; // stays pending; flushed when the peer comes back
        }
        match self.post_msg(&m.peer_id, &wire).await {
            Ok(Some(reply)) => {
                self.handle_wire(&m.peer_id, reply, None).await;
            }
            Ok(None) => self.set_status(&m.id, "sent"),
            Err(e) => log::debug!("deliver {} via http failed: {e}", m.id),
        }
    }

    /// Sends our Hello over HTTP so a peer that cannot discover us learns our address/port.
    pub(crate) async fn introduce(&self, peer_id: &str) {
        if self.links.lock().unwrap().contains_key(peer_id) {
            return;
        }
        if let Err(e) = self.post_msg(peer_id, &WsMsg::Hello { device: self.me() }).await {
            log::debug!("introduce to {peer_id} failed: {e}");
        }
    }

    pub(crate) fn send_over_link(&self, peer_id: &str, wire: &WsMsg) -> bool {
        let links = self.links.lock().unwrap();
        match links.get(peer_id) {
            Some(l) => l.tx.send(serde_json::to_string(wire).unwrap_or_default()).is_ok(),
            None => false,
        }
    }

    async fn post_msg(&self, peer_id: &str, wire: &WsMsg) -> Result<Option<WsMsg>> {
        let (ip, port) = self.peer_addr(peer_id).ok_or_else(|| anyhow!("unknown peer"))?;
        let resp = self
            .http
            .post(format!("http://{}/msg", fmt_sa(&SocketAddr::new(ip, port))))
            .header(HEADER_DEVICE_ID, self.me().id)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(wire)?)
            .timeout(Duration::from_secs(5))
            .send()
            .await?
            .error_for_status()?;
        let bytes = resp.bytes().await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        Ok(serde_json::from_slice(&bytes).ok())
    }

    pub(crate) fn set_status(&self, id: &str, status: &str) {
        if let Ok(Some(m)) = self.store.update_message(id, status, None) {
            self.emit(CoreEvent::MessageUpdate { message: m });
        }
    }

    /// Resend queued messages and resume interrupted outgoing transfers.
    ///
    /// A peer coming back triggers several flushes within a few hundred ms (peer online, link up,
    /// manual add); run concurrently they each sent every queued message again. One flush per
    /// peer at a time: a request while one runs makes it go round once more instead.
    pub(crate) async fn flush_peer(self: &Arc<Self>, peer_id: &str) {
        {
            let mut f = self.flushing.lock().unwrap();
            if let Some(again) = f.get_mut(peer_id) {
                *again = true;
                return;
            }
            f.insert(peer_id.to_string(), false);
        }
        loop {
            if self.is_online(peer_id) {
                for m in self.store.unacked_outgoing(peer_id).unwrap_or_default() {
                    self.deliver(&m).await;
                }
                for m in self.store.unfinished_files(peer_id).unwrap_or_default() {
                    let node = self.clone();
                    tokio::spawn(async move { node.run_transfer(m.id).await });
                }
            }
            let mut f = self.flushing.lock().unwrap();
            if f.get(peer_id) == Some(&true) {
                f.insert(peer_id.to_string(), false);
                continue;
            }
            f.remove(peer_id);
            break;
        }
    }

    /// Handles one protocol message from a peer. Returns an optional reply.
    pub(crate) async fn handle_wire(self: &Arc<Self>, peer_id: &str, msg: WsMsg, remote: Option<IpAddr>) -> Option<WsMsg> {
        if self.store.is_blocked(peer_id) {
            return None;
        }
        self.touch_peer(peer_id);
        match msg {
            WsMsg::Hello { device } => {
                if let Some(ip) = remote.or_else(|| self.peer_addr(peer_id).map(|a| a.0)) {
                    if device.id == peer_id {
                        self.peer_seen(device, ip);
                    }
                }
                None
            }
            // Ack only what is safely stored: the sender then stops resending. A message that
            // could not be written (disk full, database locked …) stays unacked on the sender,
            // which retries it on the next flush.
            WsMsg::Text { id, text, ts } => {
                self.store_incoming(peer_id, &id, "text", serde_json::json!({ "text": text }), "received", ts).then_some(WsMsg::Ack { id })
            }
            WsMsg::Emoji { id, emoji, ts } => {
                // custom stickers are capped at 1 MiB of Lottie JSON
                if emoji.data.as_ref().map(|d| d.len() > 1 << 20).unwrap_or(false) {
                    return Some(WsMsg::Ack { id });
                }
                let body = serde_json::to_value(&emoji).unwrap_or_default();
                self.store_incoming(peer_id, &id, "emoji", body, "received", ts).then_some(WsMsg::Ack { id })
            }
            WsMsg::FileOffer { id, transfer_id, name, size, ts } => {
                let body = FileBody { name, size, transfer_id, ..Default::default() };
                self.store_incoming(peer_id, &id, "file", serde_json::to_value(body).unwrap_or_default(), "receiving", ts);
                None
            }
            WsMsg::Typing { typing } => {
                self.emit(CoreEvent::Typing { peer_id: peer_id.into(), typing });
                None
            }
            WsMsg::Ack { id } => {
                if let Ok(Some(m)) = self.store.message(&id) {
                    if m.outgoing && m.peer_id == peer_id && m.status != "delivered" && m.kind != "file" {
                        self.set_status(&id, "delivered");
                    }
                }
                None
            }
            WsMsg::Ping => Some(WsMsg::Pong),
            WsMsg::Pong => None,
            WsMsg::Bye => {
                self.peer_bye(peer_id, "bye");
                None
            }
            WsMsg::FileCancel { transfer_id } => {
                self.peer_cancelled(peer_id, &transfer_id);
                None
            }
        }
    }

    /// Stores an incoming message (idempotent on its id). True when it is in the database now
    /// (new or already there), false when the write failed.
    pub(crate) fn store_incoming(&self, peer_id: &str, id: &str, kind: &str, body: serde_json::Value, status: &str, ts: i64) -> bool {
        let m = Message {
            id: id.into(),
            peer_id: peer_id.into(),
            outgoing: false,
            kind: kind.into(),
            body,
            status: status.into(),
            // keep the sender's clock only when it is sane, else ours
            ts: if (ts - now_ms()).abs() < 86_400_000 { ts } else { now_ms() },
        };
        if self.store.device(peer_id).is_none() {
            // message from a peer we never saw via discovery: record a stub
            let info = DeviceInfo { id: peer_id.into(), name: "Unknown".into(), os: String::new(), ver: String::new(), port: DEFAULT_PORT };
            let _ = self.store.upsert_device(&info, "", now_ms());
        }
        match self.store.insert_message(&m, true) {
            Ok(true) => {
                self.emit(CoreEvent::Message { message: m });
                self.emit_peer(peer_id);
                true
            }
            Ok(false) => true, // a resend of something we already have
            Err(e) => {
                log::warn!("storing message {id} from {peer_id} failed: {e:#}");
                false
            }
        }
    }

    // ------------------------------------------------------------------ history
    pub fn messages(&self, peer_id: &str, before_ts: Option<i64>, limit: usize) -> Result<Vec<Message>> {
        self.store.messages(peer_id, before_ts, limit)
    }

    pub fn mark_read(&self, peer_id: &str) -> Result<()> {
        self.store.mark_read(peer_id)?;
        self.emit_peer(peer_id);
        Ok(())
    }

    pub fn clear_history(&self, peer_id: Option<&str>) -> Result<()> {
        self.store.clear_history(peer_id)?;
        self.emit(CoreEvent::HistoryCleared { peer_id: peer_id.map(String::from) });
        Ok(())
    }

    pub fn delete_message(&self, id: &str) -> Result<()> {
        self.store.delete_message(id)
    }

    pub fn is_transferring(&self) -> bool {
        !self.active_out.lock().unwrap().is_empty() || !self.incoming.lock().unwrap().is_empty()
    }

    pub fn data_dir(&self) -> &Path {
        &self.cfg.data_dir
    }
}

/// Path preference when a peer answers on several addresses: LAN < overlay (CGNAT, Tailscale).
/// `ip` lies in the subnet of one of our physical network interfaces (not Tailscale, VMs,
/// containers or VPNs).
fn on_physical_lan(ip: &IpAddr) -> bool {
    let IpAddr::V4(v4) = ip else { return false };
    netutil::probe_ifaces().iter().any(|i| {
        if i.is_tailscale() || netutil::is_virtual_iface(&i.name) || i.prefix == 0 || i.prefix >= 31 {
            return false;
        }
        let mask = u32::MAX << (32 - i.prefix as u32);
        u32::from(*v4) & mask == u32::from(i.ip) & mask
    })
}

fn addr_rank(ip: &IpAddr) -> u8 {
    match ip {
        IpAddr::V4(v4) if netutil::is_cgnat(v4) => 1,
        IpAddr::V4(_) => 0,
        IpAddr::V6(_) => 2,
    }
}

pub(crate) fn fmt_sa(sa: &SocketAddr) -> String {
    match sa {
        SocketAddr::V4(v4) => v4.to_string(),
        SocketAddr::V6(v6) => format!("[{}]:{}", v6.ip(), v6.port()),
    }
}

/// reqwest without the `json` feature: small helper.
pub(crate) async fn json_body<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> Result<T> {
    let b = resp.bytes().await?;
    Ok(serde_json::from_slice(&b)?)
}
