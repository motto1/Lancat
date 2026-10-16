//! UniFFI bridge over `lancat_core::Node` for the native Android app.
//!
//! Every method maps 1:1 onto a Tauri command in `src-tauri/src/commands.rs` (same names minus the
//! IPC plumbing, same semantics), so the Kotlin side behaves like the desktop / Tauri-Android app:
//!
//! | Tauri command        | here                                 |
//! |----------------------|--------------------------------------|
//! | get_me               | `LancatNode::me`                     |
//! | list_peers           | `peers`                              |
//! | refresh_peers        | `refresh_peers` (+ `on_network_changed`) |
//! | get_messages         | `messages(peer, before_ts, limit)`   |
//! | send_text            | `send_text(peer_ids, text)`          |
//! | send_emoji           | `send_emoji(peer_ids, emoji)`        |
//! | send_files           | `send_files(peer_ids, paths)`        |
//! | send_typing          | `send_typing(peer, typing)`          |
//! | mark_read            | `mark_read`                          |
//! | retry_transfer       | `retry_transfer` (also resumes a paused one, as on desktop) |
//! | pause_transfer       | `pause_transfer`                     |
//! | cancel_transfer      | `cancel_transfer`                    |
//! | clear_history        | `clear_history(Option)`              |
//! | delete_message       | `delete_message`                     |
//! | get_settings / set_settings | `settings` / `set_settings` (typed) / `settings_json` / `set_settings_json` |
//! | set_blocked / forget_peer | same                            |
//! | add_peer             | `add_peer(addr)`                     |
//! | receive_dir / is_transferring | same                        |
//! | list/import/get/delete_sticker | same (data_dir/stickers)   |
//!
//! Events (`lancat://event` on desktop) arrive on [`CoreListener::on_event`], called from one
//! dedicated thread ("lancat-events") in order, never from a tokio worker.
//!
//! Files: the core sends plain filesystem paths (it reads them with positional reads, 4 chunks in
//! parallel, hashes them first and reopens them on retry). Android content:// URIs are copied into
//! app storage by the Kotlin side first (as the Tauri Android shell does), see LancatCoreRepository.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lancat_core::{CoreEvent, EmojiPayload, FileBody, InboundReport, Message, Node, NodeConfig, PeerView, Settings};

uniffi::setup_scaffolding!();

// ------------------------------------------------------------------------------------------ errors

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum LancatError {
    /// The core refused or failed; the message is the core's own (Chinese, user-facing) text.
    #[error("{0}")]
    Core(String),
    /// The node was shut down.
    #[error("lancat core stopped")]
    Stopped,
}

impl From<anyhow::Error> for LancatError {
    fn from(e: anyhow::Error) -> Self {
        LancatError::Core(format!("{e:#}"))
    }
}

type R<T> = Result<T, LancatError>;

// ------------------------------------------------------------------------------------------ records

/// Startup configuration (`NodeConfig`). Defaults match the desktop: TCP+UDP 47820, mDNS + UDP on.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiConfig {
    /// App-private directory for lancat.db, stickers, logs (Context.filesDir/lancat).
    pub data_dir: String,
    /// Display name override (the phone model on Android: hostname is "localhost"). None = settings / hostname.
    #[uniffi(default)]
    pub device_name: Option<String>,
    /// TCP port. 47820 = default (falls back to 47821.. when taken), 0 = OS-assigned.
    #[uniffi(default = 47820)]
    pub port: u16,
    #[uniffi(default = 47820)]
    pub udp_port: u16,
    /// Where received files are written (a real directory). None = settings.receive_dir / Downloads.
    #[uniffi(default)]
    pub receive_dir: Option<String>,
    #[uniffi(default = true)]
    pub enable_mdns: bool,
    #[uniffi(default = true)]
    pub enable_udp: bool,
    /// Extra unicast probe targets "ip:udp_port" (tests).
    #[uniffi(default)]
    pub extra_probe_targets: Vec<String>,
    /// Listen on this address only (tests). None = all interfaces.
    #[uniffi(default)]
    pub bind_ip: Option<String>,
    /// When the TCP port is taken by something else, try the next ones / an OS-assigned one.
    #[uniffi(default = true)]
    pub port_fallback: bool,
    /// Route Rust `log` to logcat (tag "lancat") on Android / stderr elsewhere. First call wins.
    #[uniffi(default = true)]
    pub init_logging: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiDevice {
    pub id: String,
    pub name: String,
    pub os: String,
    pub ver: String,
    pub port: u16,
    /// Our best LAN IPv4 (physical NIC first), "" when there is none.
    pub ip: String,
    /// Every IPv4 we probe on, best first.
    pub ips: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiPeer {
    pub id: String,
    pub name: String,
    pub os: String,
    pub ver: String,
    pub online: bool,
    pub addr: String,
    pub port: u16,
    pub blocked: bool,
    pub unread: i64,
    pub last_ts: i64,
    pub last_preview: String,
    /// Reached through this peer (its name); None when direct.
    pub via: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiEmoji {
    /// "noto" | "sticker"
    pub kind: String,
    /// codepoint ("1f600", Noto id as on desktop) or sticker id
    pub code: String,
    #[uniffi(default = "")]
    pub alt: String,
    /// Lottie JSON for custom stickers.
    #[uniffi(default)]
    pub data: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiFileBody {
    pub name: String,
    pub size: i64,
    pub transfer_id: String,
    /// Source path (outgoing) or final saved path (incoming, once done).
    pub path: String,
    pub hash: String,
    pub error: Option<String>,
    /// Machine-readable failure (`ErrCode::as_str`): peer_offline, peer_too_large, source_missing, ...
    pub code: Option<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiMessage {
    pub id: String,
    pub peer_id: String,
    pub outgoing: bool,
    /// "text" | "emoji" | "file"
    pub kind: String,
    /// outgoing: pending | sent | delivered | transferring | paused | done | failed | cancelled
    /// incoming: received | receiving | done | failed | cancelled
    pub status: String,
    /// ms, the core's monotonic message stamp (unique, ordering key)
    pub ts: i64,
    /// kind == "text"
    pub text: Option<String>,
    /// kind == "emoji"
    pub emoji: Option<FfiEmoji>,
    /// kind == "file"
    pub file: Option<FfiFileBody>,
    /// The raw body JSON (forward compatibility).
    pub body_json: String,
}

/// The settings the Android UI uses, typed. Everything else (dock, iface kinds, public access...)
/// is kept as is by `set_settings`; use `settings_json` for full fidelity.
#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSettings {
    pub device_name: String,
    pub receive_dir: String,
    /// "system" | "light" | "dark"
    pub theme: String,
    /// 0 = unlimited
    pub max_file_mb: i64,
    pub blocked_ips: Vec<String>,
    pub manual_peers: Vec<String>,
    pub recent_emoji: Vec<String>,
    pub message_sound: bool,
    pub auto_update: bool,
    pub public_access: bool,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiInboundReport {
    pub peer_id: String,
    pub peer_name: String,
    pub addr: String,
    pub port: u16,
    pub ts: i64,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct FfiSticker {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum FfiEvent {
    /// A peer appeared / changed (online, name, unread, last message, blocked).
    Peer { peer: FfiPeer },
    PeerRemoved { peer_id: String },
    /// A new message (incoming, or one we just created).
    Message { message: FfiMessage },
    /// A message's status / body changed (delivered, transfer done / failed / paused...).
    MessageUpdate { message: FfiMessage },
    Typing { peer_id: String, typing: bool },
    /// Transfer progress (both directions), throttled by the core.
    Progress { msg_id: String, peer_id: String, done: i64, total: i64, outgoing: bool },
    HistoryCleared { peer_id: Option<String> },
    Settings { settings: FfiSettings },
    /// kind = "inbound_blocked" (report set) | "recv_dir" (detail says why)
    Alert { kind: String, report: Option<FfiInboundReport>, detail: String },
    /// The event stream lagged and dropped `skipped` events: reload peers / open conversations.
    Lagged { skipped: i64 },
}

/// Implemented in Kotlin. Called on the "lancat-events" thread, in order. Keep it quick (post to a
/// coroutine / flow); a slow listener only delays later events, it never blocks the core.
#[uniffi::export(callback_interface)]
pub trait CoreListener: Send + Sync {
    fn on_event(&self, event: FfiEvent);
}

// ------------------------------------------------------------------------------------------ conversions

fn peer(p: PeerView) -> FfiPeer {
    FfiPeer {
        id: p.id,
        name: p.name,
        os: p.os,
        ver: p.ver,
        online: p.online,
        addr: p.addr,
        port: p.port,
        blocked: p.blocked,
        unread: p.unread,
        last_ts: p.last_ts,
        last_preview: p.last_preview,
        via: p.via,
    }
}

fn emoji_from(e: EmojiPayload) -> FfiEmoji {
    FfiEmoji { kind: e.kind, code: e.code, alt: e.alt, data: e.data }
}

fn emoji_to(e: FfiEmoji) -> EmojiPayload {
    EmojiPayload { kind: e.kind, code: e.code, alt: e.alt, data: e.data }
}

fn message(m: Message) -> FfiMessage {
    let (mut text, mut emoji, mut file) = (None, None, None);
    match m.kind.as_str() {
        "text" => text = m.body.get("text").and_then(|t| t.as_str()).map(String::from),
        "emoji" => emoji = serde_json::from_value::<EmojiPayload>(m.body.clone()).ok().map(emoji_from),
        "file" => {
            file = serde_json::from_value::<FileBody>(m.body.clone()).ok().map(|b| FfiFileBody {
                name: b.name,
                size: b.size as i64,
                transfer_id: b.transfer_id,
                path: b.path,
                hash: b.hash,
                error: b.error,
                code: b.code,
            })
        }
        _ => {}
    }
    FfiMessage {
        id: m.id,
        peer_id: m.peer_id,
        outgoing: m.outgoing,
        kind: m.kind,
        status: m.status,
        ts: m.ts,
        text,
        emoji,
        file,
        body_json: m.body.to_string(),
    }
}

fn settings_from(s: &Settings) -> FfiSettings {
    FfiSettings {
        device_name: s.device_name.clone(),
        receive_dir: s.receive_dir.clone(),
        theme: s.theme.clone(),
        max_file_mb: s.max_file_mb as i64,
        blocked_ips: s.blocked_ips.clone(),
        manual_peers: s.manual_peers.clone(),
        recent_emoji: s.recent_emoji.clone(),
        message_sound: s.message_sound,
        auto_update: s.auto_update,
        public_access: s.public_access,
    }
}

fn settings_apply(mut s: Settings, f: FfiSettings) -> Settings {
    s.device_name = f.device_name;
    s.receive_dir = f.receive_dir;
    s.theme = f.theme;
    s.max_file_mb = f.max_file_mb.max(0) as u64;
    s.blocked_ips = f.blocked_ips;
    s.manual_peers = f.manual_peers;
    s.recent_emoji = f.recent_emoji;
    s.message_sound = f.message_sound;
    s.auto_update = f.auto_update;
    s.public_access = f.public_access;
    s
}

fn report(r: InboundReport) -> FfiInboundReport {
    FfiInboundReport { peer_id: r.peer_id, peer_name: r.peer_name, addr: r.addr, port: r.port, ts: r.ts }
}

fn event(ev: CoreEvent) -> FfiEvent {
    match ev {
        CoreEvent::Peer { peer: p } => FfiEvent::Peer { peer: peer(p) },
        CoreEvent::PeerRemoved { peer_id } => FfiEvent::PeerRemoved { peer_id },
        CoreEvent::Message { message: m } => FfiEvent::Message { message: message(m) },
        CoreEvent::MessageUpdate { message: m } => FfiEvent::MessageUpdate { message: message(m) },
        CoreEvent::Typing { peer_id, typing } => FfiEvent::Typing { peer_id, typing },
        CoreEvent::Progress { msg_id, peer_id, done, total, outgoing } => {
            FfiEvent::Progress { msg_id, peer_id, done: done as i64, total: total as i64, outgoing }
        }
        CoreEvent::HistoryCleared { peer_id } => FfiEvent::HistoryCleared { peer_id },
        CoreEvent::Settings { settings } => FfiEvent::Settings { settings: settings_from(&settings) },
        CoreEvent::Alert { kind, report: r, detail } => FfiEvent::Alert { kind, report: r.map(report), detail },
    }
}

// ------------------------------------------------------------------------------------------ the node

/// A running Lancat core. Create once per process (it binds TCP/UDP 47820); `shutdown` when the
/// app really exits. All methods are thread-safe; the async ones are Kotlin `suspend` functions.
#[derive(uniffi::Object)]
pub struct LancatNode {
    node: Arc<Node>,
    rt: Mutex<Option<tokio::runtime::Runtime>>,
    handle: tokio::runtime::Handle,
}

impl LancatNode {
    fn alive(&self) -> R<()> {
        if self.rt.lock().unwrap().is_none() {
            return Err(LancatError::Stopped);
        }
        Ok(())
    }

    /// Runs `f` on the core's runtime and awaits it from whatever executor Kotlin polls us on
    /// (a tokio JoinHandle is executor-agnostic).
    async fn on_rt<T, F>(&self, f: F) -> R<T>
    where
        F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        self.alive()?;
        match self.handle.spawn(f).await {
            Ok(r) => r.map_err(Into::into),
            Err(e) => Err(LancatError::Core(format!("task failed: {e}"))),
        }
    }
}

#[uniffi::export]
impl LancatNode {
    /// Starts the core (store, TCP listener, discovery, housekeeping) and the event thread.
    /// Blocking (opens the DB, binds ports): call it off the main thread.
    #[uniffi::constructor]
    pub fn start(config: FfiConfig, listener: Box<dyn CoreListener>) -> R<Arc<Self>> {
        if config.init_logging {
            logging::init();
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("lancat-core")
            .enable_all()
            .build()
            .map_err(|e| LancatError::Core(format!("tokio: {e}")))?;
        let mut cfg = NodeConfig::new(PathBuf::from(&config.data_dir));
        cfg.port = config.port;
        cfg.udp_port = config.udp_port;
        cfg.port_fallback = config.port_fallback;
        cfg.enable_mdns = config.enable_mdns;
        cfg.enable_udp = config.enable_udp;
        cfg.name_override = config.device_name.filter(|n| !n.trim().is_empty());
        cfg.recv_dir_override = config.receive_dir.filter(|d| !d.trim().is_empty()).map(PathBuf::from);
        cfg.extra_probe_targets = config.extra_probe_targets.iter().filter_map(|a| a.trim().parse().ok()).collect();
        cfg.bind_ip = config.bind_ip.and_then(|ip| ip.trim().parse().ok());

        // same fallback as the Tauri Android shell: a taken port must not keep the app from starting
        let node = rt.block_on(async {
            match Node::start(cfg.clone()).await {
                Ok(n) => Ok(n),
                Err(e) if cfg.port_fallback && cfg.port != 0 => {
                    log::warn!("{e:#}; falling back to a random port");
                    let mut c = cfg;
                    c.port = 0;
                    Node::start(c).await
                }
                Err(e) => Err(e),
            }
        })?;

        // events: one ordered stream to Kotlin on its own thread
        let mut rx = node.subscribe();
        std::thread::Builder::new()
            .name("lancat-events".into())
            .spawn(move || loop {
                match rx.blocking_recv() {
                    Ok(ev) => listener.on_event(event(ev)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        listener.on_event(FfiEvent::Lagged { skipped: n as i64 })
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            })
            .map_err(|e| LancatError::Core(format!("event thread: {e}")))?;

        let handle = rt.handle().clone();
        Ok(Arc::new(LancatNode { node, rt: Mutex::new(Some(rt)), handle }))
    }

    // ---------------------------------------------------------------- identity / peers

    pub fn me(&self) -> FfiDevice {
        let d = self.node.me();
        let ips = local_ips();
        FfiDevice { id: d.id, name: d.name, os: d.os, ver: d.ver, port: d.port, ip: ips.first().cloned().unwrap_or_default(), ips }
    }

    pub fn port(&self) -> u16 {
        self.node.port()
    }

    /// Sorted like the desktop list: online first, then most recent conversation, then name.
    pub fn peers(&self) -> Vec<FfiPeer> {
        self.node.peers().into_iter().map(peer).collect()
    }

    pub fn peer(&self, peer_id: String) -> Option<FfiPeer> {
        self.node.peer_view(&peer_id).map(peer)
    }

    /// Manual refresh: one discovery probe burst on every interface.
    pub fn refresh_peers(&self) {
        self.node.refresh_peers()
    }

    /// Android ConnectivityManager callback (network available / lost / link properties changed):
    /// probe right away instead of waiting for the core's 5 s interface poll. Reconnects follow
    /// from the probe replies (housekeeping redials peers that are online without a link).
    pub fn on_network_changed(&self) {
        self.node.refresh_peers()
    }

    /// Manually add a peer by "ip[:port]" (Wi-Fi with client isolation, Tailscale). Like the
    /// desktop's empty-state form this does not save it; add it to `manual_peers` for that.
    pub async fn add_peer(&self, addr: String) -> R<FfiPeer> {
        let node = self.node.clone();
        self.on_rt(async move { node.add_peer_addr(&addr).await }).await.map(peer)
    }

    pub fn set_blocked(&self, peer_id: String, blocked: bool) -> R<()> {
        Ok(self.node.set_blocked(&peer_id, blocked)?)
    }

    pub fn forget_peer(&self, peer_id: String) -> R<()> {
        Ok(self.node.forget_peer(&peer_id)?)
    }

    pub fn inbound_reports(&self) -> Vec<FfiInboundReport> {
        self.node.inbound_reports().into_iter().map(report).collect()
    }

    pub fn clear_inbound_reports(&self) {
        self.node.clear_inbound_reports()
    }

    // ---------------------------------------------------------------- messages

    /// History page, ascending by ts: the `limit` newest messages older than `before_ts` (None = newest).
    pub fn messages(&self, peer_id: String, before_ts: Option<i64>, limit: u32) -> R<Vec<FfiMessage>> {
        Ok(self.node.messages(&peer_id, before_ts, limit as usize)?.into_iter().map(message).collect())
    }

    pub async fn send_text(&self, peer_ids: Vec<String>, text: String) -> R<Vec<FfiMessage>> {
        let node = self.node.clone();
        let out = self
            .on_rt(async move {
                let mut out = vec![];
                for p in peer_ids {
                    out.push(node.send_text(&p, &text).await?);
                }
                Ok(out)
            })
            .await?;
        Ok(out.into_iter().map(message).collect())
    }

    pub async fn send_emoji(&self, peer_ids: Vec<String>, emoji: FfiEmoji) -> R<Vec<FfiMessage>> {
        let node = self.node.clone();
        let emoji = emoji_to(emoji);
        let out = self
            .on_rt(async move {
                let mut out = vec![];
                for p in peer_ids {
                    out.push(node.send_emoji(&p, emoji.clone()).await?);
                }
                Ok(out)
            })
            .await?;
        Ok(out.into_iter().map(message).collect())
    }

    /// Files and folders (recursively) by filesystem path, to each peer. Returns the file
    /// messages right away; the transfers run in the background (Progress / MessageUpdate events).
    pub async fn send_files(&self, peer_ids: Vec<String>, paths: Vec<String>) -> R<Vec<FfiMessage>> {
        let node = self.node.clone();
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let out = self.on_rt(async move { node.send_files(&peer_ids, &paths).await }).await?;
        Ok(out.into_iter().map(message).collect())
    }

    pub fn send_typing(&self, peer_id: String, typing: bool) {
        self.node.send_typing(&peer_id, typing)
    }

    pub fn mark_read(&self, peer_id: String) -> R<()> {
        Ok(self.node.mark_read(&peer_id)?)
    }

    /// Retries a failed outgoing transfer, or resumes a paused one, where the receiver left off.
    pub async fn retry_transfer(&self, msg_id: String) -> R<()> {
        let node = self.node.clone();
        self.on_rt(async move { node.retry_transfer(&msg_id).await }).await
    }

    pub fn pause_transfer(&self, msg_id: String) -> R<()> {
        Ok(self.node.pause_transfer(&msg_id)?)
    }

    /// Cancels a transfer in either direction and tells the other side.
    pub fn cancel_transfer(&self, msg_id: String) -> R<()> {
        Ok(self.node.cancel_transfer(&msg_id)?)
    }

    /// None = every conversation.
    pub fn clear_history(&self, peer_id: Option<String>) -> R<()> {
        Ok(self.node.clear_history(peer_id.as_deref())?)
    }

    /// Deletes one message locally (no event is emitted, as on desktop).
    pub fn delete_message(&self, msg_id: String) -> R<()> {
        Ok(self.node.delete_message(&msg_id)?)
    }

    pub fn is_transferring(&self) -> bool {
        self.node.is_transferring()
    }

    pub fn receive_dir(&self) -> String {
        self.node.receive_dir().to_string_lossy().to_string()
    }

    /// Creates the receive folder and test-writes it: Ok(dir) or Err(why).
    pub fn check_receive_dir(&self) -> R<String> {
        self.node.check_receive_dir().map(|p| p.to_string_lossy().to_string()).map_err(LancatError::Core)
    }

    pub fn data_dir(&self) -> String {
        self.node.data_dir().to_string_lossy().to_string()
    }

    // ---------------------------------------------------------------- settings

    pub fn settings(&self) -> FfiSettings {
        settings_from(&self.node.settings())
    }

    /// Writes the typed fields over the current settings (the rest stays), like desktop set_settings.
    pub fn set_settings(&self, settings: FfiSettings) -> R<FfiSettings> {
        let s = settings_apply(self.node.settings(), settings);
        Ok(settings_from(&self.node.set_settings(s)?))
    }

    /// The full core settings as JSON (snake_case keys, as the desktop frontend sees them).
    pub fn settings_json(&self) -> String {
        serde_json::to_string(&self.node.settings()).unwrap_or_default()
    }

    /// Replaces the full core settings from JSON (missing keys take their defaults).
    pub fn set_settings_json(&self, json: String) -> R<String> {
        let s: Settings = serde_json::from_str(&json).map_err(|e| LancatError::Core(format!("settings json: {e}")))?;
        let s = self.node.set_settings(s)?;
        Ok(serde_json::to_string(&s).unwrap_or_default())
    }

    // ---------------------------------------------------------------- stickers (.tgs / Lottie), as desktop

    pub fn list_stickers(&self) -> Vec<FfiSticker> {
        stickers::list(&self.sticker_dir())
    }

    /// Saves a Lottie JSON (already gunzipped from .tgs). Max 1 MiB.
    pub fn import_sticker(&self, name: String, lottie: String) -> R<FfiSticker> {
        stickers::import(&self.sticker_dir(), &name, &lottie).map_err(LancatError::Core)
    }

    pub fn get_sticker(&self, id: String) -> R<String> {
        stickers::get(&self.sticker_dir(), &id).map_err(LancatError::Core)
    }

    pub fn delete_sticker(&self, id: String) -> R<()> {
        stickers::delete(&self.sticker_dir(), &id).map_err(LancatError::Core)
    }

    // ---------------------------------------------------------------- lifecycle

    /// Clean exit: tells peers we are leaving, stops discovery/links and the runtime. Idempotent.
    /// The object is unusable afterwards (methods return Stopped / do nothing).
    pub fn shutdown(&self) {
        self.node.shutdown();
        if let Some(rt) = self.rt.lock().unwrap().take() {
            // let the bye frames flush, then drop the workers without blocking the caller long
            rt.shutdown_timeout(std::time::Duration::from_millis(800));
        }
    }
}

impl LancatNode {
    fn sticker_dir(&self) -> PathBuf {
        self.node.data_dir().join("stickers")
    }
}

/// Our IPv4 addresses, physical LAN first (what the user would type on the other device).
fn local_ips() -> Vec<String> {
    let mut v = lancat_core::netutil::probe_ifaces();
    v.sort_by_key(|i| (i.virt, i.l3, i.ip));
    let mut out: Vec<String> = v.into_iter().map(|i| i.ip.to_string()).collect();
    out.dedup();
    out
}

/// Exposed for tests and the Kotlin side: the core version this library was built from.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// blake3 of a file, hex (same hash the core uses to verify transfers). For tests / diagnostics.
#[uniffi::export]
pub fn hash_file(path: String) -> R<String> {
    Ok(lancat_core::files::hash_file(std::path::Path::new(&path))?)
}

mod stickers {
    use super::FfiSticker;
    use std::path::Path;

    fn ok_id(id: &str) -> Result<(), String> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("bad id".into());
        }
        Ok(())
    }

    pub fn list(dir: &Path) -> Vec<FfiSticker> {
        let mut v: Vec<(std::time::SystemTime, FfiSticker)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|ent| {
                let p = ent.path();
                if p.extension()? != "json" {
                    return None;
                }
                let id = p.file_stem()?.to_string_lossy().to_string();
                let name = std::fs::read_to_string(dir.join(format!("{id}.name"))).unwrap_or_else(|_| id.clone());
                Some((ent.metadata().ok()?.modified().ok()?, FfiSticker { id, name }))
            })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.into_iter().map(|x| x.1).collect()
    }

    pub fn import(dir: &Path, name: &str, lottie: &str) -> Result<FfiSticker, String> {
        if lottie.len() > 1 << 20 {
            return Err("贴纸太大（>1MB）".into());
        }
        let v: serde_json::Value = serde_json::from_str(lottie).map_err(|_| "不是有效的 Lottie JSON".to_string())?;
        if v.get("layers").is_none() {
            return Err("不是有效的 Lottie 动画".into());
        }
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        std::fs::write(dir.join(format!("{id}.json")), lottie).map_err(|e| e.to_string())?;
        let name: String = name.chars().take(64).collect();
        std::fs::write(dir.join(format!("{id}.name")), &name).map_err(|e| e.to_string())?;
        Ok(FfiSticker { id, name })
    }

    pub fn get(dir: &Path, id: &str) -> Result<String, String> {
        ok_id(id)?;
        std::fs::read_to_string(dir.join(format!("{id}.json"))).map_err(|e| e.to_string())
    }

    pub fn delete(dir: &Path, id: &str) -> Result<(), String> {
        ok_id(id)?;
        let _ = std::fs::remove_file(dir.join(format!("{id}.name")));
        std::fs::remove_file(dir.join(format!("{id}.json"))).map_err(|e| e.to_string())
    }
}

mod logging {
    //! Rust `log` → logcat (tag "lancat", `adb logcat -s lancat`) on Android, stderr elsewhere.
    pub fn init() {
        struct L;
        impl log::Log for L {
            fn enabled(&self, m: &log::Metadata) -> bool {
                m.level() <= log::Level::Info || (m.level() <= log::Level::Debug && m.target().starts_with("lancat"))
            }
            fn log(&self, r: &log::Record) {
                if !self.enabled(r.metadata()) {
                    return;
                }
                emit(r.level(), &format!("{}: {}", r.target(), r.args()));
            }
            fn flush(&self) {}
        }
        static LOGGER: L = L;
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Debug);
        }
    }

    #[cfg(target_os = "android")]
    fn emit(level: log::Level, msg: &str) {
        #[link(name = "log")]
        extern "C" {
            fn __android_log_write(prio: i32, tag: *const std::ffi::c_char, text: *const std::ffi::c_char) -> i32;
        }
        let prio = match level {
            log::Level::Error => 6,
            log::Level::Warn => 5,
            log::Level::Info => 4,
            _ => 3,
        };
        let msg = std::ffi::CString::new(msg.replace('\0', " ")).unwrap_or_default();
        unsafe { __android_log_write(prio, c"lancat".as_ptr(), msg.as_ptr()) };
    }

    #[cfg(not(target_os = "android"))]
    fn emit(level: log::Level, msg: &str) {
        if std::env::var_os("LANCAT_FFI_QUIET").is_none() {
            eprintln!("[{level}] {msg}");
        }
    }
}
