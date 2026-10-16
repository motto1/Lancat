//! Links over the public internet: a device exposes its TCP port through a tunnel (Cloudflare
//! Tunnel, frp, port forwarding) and another one types that address (+ the pair code) in.
//!
//! * The dialing side keeps one WebSocket per address in `Settings::remote_peers` (wss:// for
//!   https tunnels), reconnecting with backoff. Its first handshake presents `code:<pair code>`,
//!   the answer carries a token (`HEADER_TOKEN`) used from then on.
//! * The accepting side treats every connection that comes through a tunnel (forwarding headers,
//!   loopback while public access is on, or a non-LAN source) as public: only `/ws` is served,
//!   and only with a valid code or token. Wrong codes are rate limited.
//! * Both sides keep the peer's address as 0.0.0.0 (`Node::is_remote`): there is nothing to dial
//!   or probe, so messages and files go over the link (the link transfer path already exists for
//!   receivers that cannot be reached inbound).

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use axum::http::HeaderMap;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::events::CoreEvent;
use crate::node::Node;
use crate::protocol::*;
use crate::store::{PairedRemote, RemotePeer};

/// The placeholder address of a public peer.
pub(crate) const REMOTE_IP: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
/// Wrong pair codes allowed per minute (over all sources) before codes are refused for a while.
const MAX_FAILS_PER_MIN: usize = 8;

/// What a link is, for the checks after the hello.
#[derive(Debug, Clone)]
pub(crate) enum LinkCtx {
    Lan,
    /// We dialed this public address (`RemotePeer::addr`).
    RemoteDial { addr: String },
    /// A public connection came in. `bound`: the device its token belongs to; `new_token`: it
    /// paired with the code just now and this token was handed out.
    RemoteAccept { bound: Option<String>, new_token: Option<String> },
}

/// Turns what the user typed into the WebSocket URL to dial. `None`: a plain LAN address
/// (IPv4 with or without the default port), handled by the manual-peer path as before.
pub fn remote_ws_url(addr: &str) -> Option<String> {
    let a = addr.trim().trim_end_matches('/');
    if a.is_empty() {
        return None;
    }
    let (scheme, rest) = match a.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r.to_string()),
        None => (String::new(), a.to_string()),
    };
    if scheme.is_empty() {
        if a.parse::<IpAddr>().is_ok() {
            return None;
        }
        if let Ok(sa) = a.parse::<std::net::SocketAddr>() {
            // 47820..=47829: a Lancat that moved off a taken port, still a LAN address
            if (DEFAULT_PORT..=DEFAULT_PORT.saturating_add(9)).contains(&sa.port()) {
                return None;
            }
            return Some(format!("ws://{a}/ws"));
        }
        // host or host:port
        let host = rest.split('/').next().unwrap_or("");
        if host.is_empty() || host.contains(char::is_whitespace) {
            return None;
        }
        let with_port = if host.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) { host.to_string() } else { format!("{host}:{DEFAULT_PORT}") };
        return Some(format!("ws://{with_port}/ws"));
    }
    let ws = match scheme.as_str() {
        "https" | "wss" => "wss",
        "http" | "ws" => "ws",
        _ => return None,
    };
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (rest.as_str(), String::new()),
    };
    if host.is_empty() {
        return None;
    }
    let path = if path.is_empty() || path == "/" { "/ws".to_string() } else if path.ends_with("/ws") { path } else { format!("{path}/ws") };
    Some(format!("{ws}://{host}{path}"))
}

/// "K7P2-9QXM": 8 symbols of Crockford base32 (no I L O U), ~40 bits.
fn new_pair_code() -> String {
    const A: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let b = uuid::Uuid::new_v4().into_bytes();
    let s: String = b.iter().take(8).map(|x| A[(*x as usize) % 32] as char).collect();
    format!("{}-{}", &s[..4], &s[4..])
}

fn norm_code(c: &str) -> String {
    c.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).map(|c| match c {
        'O' => '0',
        'I' | 'L' => '1',
        c => c,
    }).collect()
}

fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

impl Node {
    // ------------------------------------------------------------------ accepting side

    /// Whether a request came through a tunnel / from outside (see the module docs).
    pub(crate) fn is_public_request(&self, ip: &IpAddr, h: &HeaderMap) -> bool {
        let forwarded = ["cf-connecting-ip", "cf-ray", "x-forwarded-for", "x-real-ip", "forwarded"].iter().any(|k| h.contains_key(*k));
        forwarded || h.contains_key(HEADER_AUTH) || !self.is_trusted_source(ip) || (ip.is_loopback() && self.store.settings().public_access)
    }

    /// Checks a public handshake's `HEADER_AUTH`. Ok: the link context to run it with.
    pub(crate) fn check_public_auth(&self, h: &HeaderMap) -> std::result::Result<LinkCtx, &'static str> {
        let st = self.store.settings();
        if !st.public_access {
            return Err("public access is off");
        }
        let auth = h.get(HEADER_AUTH).and_then(|v| v.to_str().ok()).unwrap_or("");
        if let Some(t) = auth.strip_prefix("token:") {
            if let Some(p) = st.paired_remotes.iter().find(|p| !p.token.is_empty() && p.token == t) {
                return Ok(LinkCtx::RemoteAccept { bound: Some(p.id.clone()), new_token: None });
            }
            return Err("unknown token");
        }
        if let Some(c) = auth.strip_prefix("code:") {
            {
                let mut f = self.pair_fails.lock().unwrap();
                while f.front().is_some_and(|t| t.elapsed() > Duration::from_secs(60)) {
                    f.pop_front();
                }
                if f.len() >= MAX_FAILS_PER_MIN {
                    return Err("too many wrong codes, wait a minute");
                }
                if st.pair_code.is_empty() || norm_code(c) != norm_code(&st.pair_code) {
                    f.push_back(Instant::now());
                    return Err("wrong pair code");
                }
            }
            return Ok(LinkCtx::RemoteAccept { bound: None, new_token: Some(new_token()) });
        }
        Err("pairing required")
    }

    /// After the hello: binds / records public peers. false: drop the link.
    pub(crate) fn link_ctx_ok(&self, ctx: &LinkCtx, device: &DeviceInfo) -> bool {
        match ctx {
            LinkCtx::Lan => true,
            LinkCtx::RemoteAccept { bound: Some(id), .. } => *id == device.id,
            LinkCtx::RemoteAccept { bound: None, new_token } => {
                let Some(tok) = new_token else { return false };
                let (id, name, tok) = (device.id.clone(), device.name.clone(), tok.clone());
                self.update_settings(|s| {
                    s.paired_remotes.retain(|p| p.id != id);
                    s.paired_remotes.push(PairedRemote { id, name, token: tok });
                });
                log::info!("public pairing: {} ({}) paired with the code", device.name, device.id);
                true
            }
            LinkCtx::RemoteDial { addr } => {
                let (addr, id, name) = (addr.clone(), device.id.clone(), device.name.clone());
                self.update_settings(|s| {
                    if let Some(r) = s.remote_peers.iter_mut().find(|r| r.addr == addr) {
                        r.peer_id = id;
                        r.name = name;
                    }
                });
                true
            }
        }
    }

    fn update_settings(&self, f: impl FnOnce(&mut crate::store::Settings)) {
        let mut s = self.store.settings();
        let old = s.clone();
        f(&mut s);
        if s != old {
            match self.store.save_settings(&s) {
                Ok(()) => self.emit(CoreEvent::Settings { settings: s }),
                Err(e) => log::warn!("saving settings: {e}"),
            }
        }
    }

    // ------------------------------------------------------------------ settings API

    /// Switches accepting public links on/off; a pair code is made the first time.
    pub fn set_public_access(&self, on: bool) -> String {
        self.update_settings(|s| {
            s.public_access = on;
            if on && s.pair_code.is_empty() {
                s.pair_code = new_pair_code();
            }
        });
        self.store.settings().pair_code
    }

    /// A new pair code (devices already paired keep working).
    pub fn regen_pair_code(&self) -> String {
        self.update_settings(|s| s.pair_code = new_pair_code());
        self.store.settings().pair_code
    }

    /// Revokes a paired public device (its token stops working; an open link is closed).
    pub fn unpair_remote(&self, id: &str) {
        self.update_settings(|s| s.paired_remotes.retain(|p| p.id != id));
        if self.is_remote(id) {
            self.links.lock().unwrap().remove(id);
        }
    }

    /// Adds a public address to dial, with the pair code the other side shows.
    pub fn add_remote_peer(&self, addr: &str, code: &str) -> Result<()> {
        let addr = addr.trim().to_string();
        remote_ws_url(&addr).ok_or_else(|| anyhow!("无效地址: {addr}"))?;
        if code.trim().is_empty() && !self.store.settings().remote_peers.iter().any(|r| r.addr == addr && !r.token.is_empty()) {
            bail!("请在地址后面加上对方的配对码");
        }
        let code = code.trim().to_string();
        self.update_settings(|s| match s.remote_peers.iter_mut().find(|r| r.addr == addr) {
            Some(r) => {
                if !code.is_empty() {
                    r.code = code;
                    r.token.clear();
                }
            }
            None => s.remote_peers.push(RemotePeer { addr, code, ..Default::default() }),
        });
        self.remote_kick.notify_one();
        Ok(())
    }

    pub fn remove_remote_peer(&self, addr: &str) {
        let mut gone = None;
        self.update_settings(|s| {
            if let Some(r) = s.remote_peers.iter().find(|r| r.addr == addr) {
                gone = Some(r.peer_id.clone());
            }
            s.remote_peers.retain(|r| r.addr != addr);
        });
        if let Some(id) = gone.filter(|i| !i.is_empty() && self.is_remote(i)) {
            self.links.lock().unwrap().remove(&id);
        }
    }

    // ------------------------------------------------------------------ dialing side

    pub(crate) fn spawn_remote(self: Arc<Self>) {
        let mut stop = self.shutdown_rx();
        tokio::spawn(async move {
            // addr -> (next attempt, current backoff)
            let mut wait: std::collections::HashMap<String, (Instant, u64)> = Default::default();
            loop {
                let list = self.store.settings().remote_peers;
                for r in list {
                    let linked = !r.peer_id.is_empty() && self.links.lock().unwrap().contains_key(&r.peer_id);
                    if linked {
                        wait.remove(&r.addr);
                        continue;
                    }
                    if r.code.is_empty() && r.token.is_empty() {
                        continue; // pairing failed: waits for a new code from the user
                    }
                    if wait.get(&r.addr).is_some_and(|(t, _)| Instant::now() < *t) {
                        continue;
                    }
                    if !self.remote_busy.lock().unwrap().insert(r.addr.clone()) {
                        continue;
                    }
                    let back = wait.get(&r.addr).map(|w| w.1).unwrap_or(2);
                    wait.insert(r.addr.clone(), (Instant::now() + Duration::from_secs(back), (back * 2).min(60)));
                    let node = self.clone();
                    tokio::spawn(async move {
                        let a = r.addr.clone();
                        if let Err(e) = node.clone().remote_connect(r).await {
                            log::info!("public address {a}: {e}");
                        }
                        node.remote_busy.lock().unwrap().remove(&a);
                    });
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                    _ = self.remote_kick.notified() => { wait.clear(); }
                    _ = stop.changed() => break,
                }
            }
        });
    }

    async fn remote_connect(self: Arc<Self>, r: RemotePeer) -> Result<()> {
        let url = remote_ws_url(&r.addr).ok_or_else(|| anyhow!("invalid address"))?;
        let mut req = url.as_str().into_client_request()?;
        req.headers_mut().insert(HEADER_DEVICE_ID, self.me().id.parse()?);
        let auth = if !r.token.is_empty() { format!("token:{}", r.token) } else { format!("code:{}", r.code) };
        req.headers_mut().insert(HEADER_AUTH, auth.parse()?);
        use tokio_tungstenite::tungstenite::Error as WsErr;
        let res = tokio::time::timeout(Duration::from_secs(12), tokio_tungstenite::connect_async_with_config(req, None, true)).await.map_err(|_| anyhow!("timeout"))?;
        let (ws, resp) = match res {
            Ok(x) => x,
            Err(WsErr::Http(resp)) if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 => {
                let why = resp.body().as_ref().map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default();
                // a refused token or code will not start working by itself: stop until the user acts
                let addr = r.addr.clone();
                self.update_settings(|s| {
                    if let Some(x) = s.remote_peers.iter_mut().find(|x| x.addr == addr) {
                        x.code.clear();
                        x.token.clear();
                    }
                });
                let detail = if why.contains("wrong") {
                    format!("{}：配对码不对，请重新输入", r.addr)
                } else if why.contains("off") {
                    format!("{}：对方没有打开「允许公网连接」", r.addr)
                } else if why.contains("too many") {
                    format!("{}：配对码错误次数太多，等一分钟再试", r.addr)
                } else {
                    format!("{}：对方拒绝了连接（{why}），请重新输入配对码", r.addr)
                };
                self.emit(CoreEvent::Alert { kind: "remote_auth".into(), report: None, detail });
                bail!("refused: {why}");
            }
            Err(e) => bail!("{e}"),
        };
        if let Some(tok) = resp.headers().get(HEADER_TOKEN).and_then(|v| v.to_str().ok()) {
            let (addr, tok) = (r.addr.clone(), tok.to_string());
            self.update_settings(|s| {
                if let Some(x) = s.remote_peers.iter_mut().find(|x| x.addr == addr) {
                    x.token = tok;
                    x.code.clear();
                }
            });
        }
        log::info!("public address {}: connected", r.addr);
        let expected = (!r.peer_id.is_empty()).then(|| r.peer_id.clone());
        self.spawn_client_link(ws, expected, REMOTE_IP, LinkCtx::RemoteDial { addr: r.addr });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(remote_ws_url("192.168.1.2"), None);
        assert_eq!(remote_ws_url("192.168.1.2:47820"), None);
        assert_eq!(remote_ws_url("1.2.3.4:7000").as_deref(), Some("ws://1.2.3.4:7000/ws"));
        assert_eq!(remote_ws_url("frp.example.com:7000").as_deref(), Some("ws://frp.example.com:7000/ws"));
        assert_eq!(remote_ws_url("frp.example.com").as_deref(), Some("ws://frp.example.com:47820/ws"));
        assert_eq!(remote_ws_url("https://lancat.example.com/").as_deref(), Some("wss://lancat.example.com/ws"));
        assert_eq!(remote_ws_url("http://a.b:8080").as_deref(), Some("ws://a.b:8080/ws"));
        assert_eq!(remote_ws_url("https://a.b/lancat").as_deref(), Some("wss://a.b/lancat/ws"));
    }

    #[test]
    fn codes() {
        let c = new_pair_code();
        assert_eq!(c.len(), 9);
        assert_eq!(norm_code(&c.to_lowercase().replace('-', " ")), norm_code(&c));
        assert_eq!(norm_code("k7p2-9qxo"), "K7P29QX0");
    }

    use crate::events::CoreEvent;
    use crate::node::NodeConfig;
    use std::sync::Arc;

    async fn start(dir: &std::path::Path, name: &str) -> Arc<Node> {
        let mut cfg = NodeConfig::new(dir.join("data"));
        cfg.port = 0;
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
        cfg.name_override = Some(name.into());
        cfg.recv_dir_override = Some(dir.join("recv"));
        Node::start(cfg).await.unwrap()
    }

    async fn until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(secs) {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// Pair over the public path (a non-default port = "remote" address), then messages and files
    /// both ways over the link only, then the token, then revocation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn public_pairing_end_to_end() {
        let th = tempfile::tempdir().unwrap();
        let tg = tempfile::tempdir().unwrap();
        let host = start(th.path(), "host").await;
        let guest = start(tg.path(), "guest").await;
        let addr = format!("127.0.0.1:{}", host.port());
        assert!(remote_ws_url(&addr).is_some(), "test port must not be the default");

        // public access off: refused
        guest.add_remote_peer(&addr, "AAAA-BBBB").unwrap();
        assert!(until(8, || guest.settings().remote_peers[0].code.is_empty()).await, "refused while off");
        assert!(host.settings().paired_remotes.is_empty());

        // wrong code: refused
        let code = host.set_public_access(true);
        guest.add_remote_peer(&addr, "ZZZZ-ZZZZ").unwrap();
        assert!(until(8, || guest.settings().remote_peers[0].code.is_empty()).await, "wrong code refused");
        assert!(host.settings().paired_remotes.is_empty());
        // plain HTTP routes from a public-looking request are closed
        let r = reqwest::Client::new().post(format!("http://{addr}/msg")).header(HEADER_AUTH, "token:x").body("{}").send().await.unwrap();
        assert_eq!(r.status().as_u16(), 403);

        // right code (lower case, no dash): paired
        guest.add_remote_peer(&addr, &code.to_lowercase().replace('-', "")).unwrap();
        let hid = host.me().id;
        let gid = guest.me().id;
        assert!(until(10, || guest.links.lock().unwrap().contains_key(&hid) && host.links.lock().unwrap().contains_key(&gid)).await, "link up");
        assert!(!guest.settings().remote_peers[0].token.is_empty(), "token saved");
        assert_eq!(host.settings().paired_remotes[0].id, gid);
        assert!(guest.is_remote(&hid) && host.is_remote(&gid));

        // text both ways
        let a = guest.send_text(&hid, "hi from guest").await.unwrap();
        let b = host.send_text(&gid, "hi from host").await.unwrap();
        assert!(until(8, || guest.store.message(&a.id).unwrap().unwrap().status == "delivered" && host.store.message(&b.id).unwrap().unwrap().status == "delivered").await, "texts delivered");

        // files both ways (link only)
        for (snd, rcv_id, dir, rdir) in [(&guest, &hid, tg.path(), th.path()), (&host, &gid, th.path(), tg.path())] {
            let data: Vec<u8> = (0..(5 << 20) + 777).map(|i| (i * 13 % 251) as u8).collect();
            let src = dir.join("f.bin");
            std::fs::write(&src, &data).unwrap();
            let mut ev = snd.subscribe();
            let ms = snd.send_files(&[rcv_id.clone()], &[src]).await.unwrap();
            let id = ms[0].id.clone();
            let m = tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    if let Ok(CoreEvent::MessageUpdate { message }) = ev.recv().await {
                        if message.id == id && matches!(message.status.as_str(), "done" | "failed") {
                            return message;
                        }
                    }
                }
            })
            .await
            .expect("transfer timed out");
            assert_eq!(m.status, "done", "{:?}", m.body);
            assert!(std::fs::read(rdir.join("recv").join("f.bin")).unwrap() == data);
        }

        // drop the link: the guest reconnects with its token (no code)
        host.links.lock().unwrap().remove(&gid);
        guest.links.lock().unwrap().remove(&hid);
        assert!(until(15, || guest.links.lock().unwrap().contains_key(&hid)).await, "reconnected with token");
        assert!(guest.settings().remote_peers[0].code.is_empty());

        // revoke: the token stops working
        host.unpair_remote(&gid);
        guest.links.lock().unwrap().remove(&hid);
        assert!(until(15, || guest.settings().remote_peers[0].token.is_empty()).await, "revoked token refused");
    }

    /// Real tunnel (manual): `LANCAT_TUNNEL_URL=https://x.trycloudflare.com LANCAT_TUNNEL_PORT=47931
    /// cargo test -p lancat-core public_via_tunnel -- --ignored`, with `cloudflared tunnel --url
    /// http://localhost:47931` running.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn public_via_tunnel() {
        let _ = env_logger::builder().is_test(true).try_init();
        let url = std::env::var("LANCAT_TUNNEL_URL").unwrap();
        let port: u16 = std::env::var("LANCAT_TUNNEL_PORT").unwrap().parse().unwrap();
        let th = tempfile::tempdir().unwrap();
        let tg = tempfile::tempdir().unwrap();
        let mut cfg = NodeConfig::new(th.path().join("data"));
        cfg.port = port;
        cfg.port_fallback = false;
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
        cfg.name_override = Some("host".into());
        cfg.recv_dir_override = Some(th.path().join("recv"));
        let host = Node::start(cfg).await.unwrap();
        let guest = start(tg.path(), "guest").await;
        let code = host.set_public_access(true);
        guest.add_remote_peer(&url, &code).unwrap();
        let (hid, gid) = (host.me().id, guest.me().id);
        let t0 = Instant::now();
        assert!(until(40, || guest.links.lock().unwrap().contains_key(&hid) && host.links.lock().unwrap().contains_key(&gid)).await, "link up over the tunnel: {:?}", guest.settings().remote_peers);
        eprintln!("TUNNEL link up after {:?}", t0.elapsed());
        let a = guest.send_text(&hid, "over the tunnel").await.unwrap();
        let b = host.send_text(&gid, "back over the tunnel").await.unwrap();
        assert!(until(15, || guest.store.message(&a.id).unwrap().unwrap().status == "delivered" && host.store.message(&b.id).unwrap().unwrap().status == "delivered").await, "texts");
        for (snd, rcv_id, dir, rdir) in [(&guest, &hid, tg.path(), th.path()), (&host, &gid, th.path(), tg.path())] {
            let data: Vec<u8> = (0..(6 << 20) + 321).map(|i| (i * 31 % 251) as u8).collect();
            let src = dir.join("t.bin");
            std::fs::write(&src, &data).unwrap();
            let mut ev = snd.subscribe();
            let t1 = Instant::now();
            for n in [snd, if *rcv_id == hid { &host } else { &guest }] {
                let mut e = n.subscribe();
                let who = n.me().name;
                tokio::spawn(async move {
                    let mut last = 0u64;
                    while let Ok(x) = e.recv().await {
                        if let CoreEvent::Progress { done, total, outgoing, .. } = x {
                            if done >= last + (512 << 10) || done == total {
                                last = done;
                                eprintln!("TUNNEL {who} {} {done}/{total} at {:?}", if outgoing { "out" } else { "in" }, t1.elapsed());
                            }
                        }
                    }
                });
            }
            let ms = snd.send_files(&[rcv_id.clone()], &[src]).await.unwrap();
            let id = ms[0].id.clone();
            let m = tokio::time::timeout(Duration::from_secs(180), async {
                loop {
                    if let Ok(CoreEvent::MessageUpdate { message }) = ev.recv().await {
                        if message.id == id && matches!(message.status.as_str(), "done" | "failed") {
                            return message;
                        }
                    }
                }
            })
            .await
            .expect("tunnel transfer timed out");
            assert_eq!(m.status, "done", "{:?}", m.body);
            assert!(std::fs::read(rdir.join("recv").join("t.bin")).unwrap() == data);
            eprintln!("TUNNEL 9 MiB file in {:?}", t1.elapsed());
        }
    }
}
