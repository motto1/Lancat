//! Reaching a peer through another one, automatically.
//!
//! Every link carries a small gossip of "peers I have a link to right now" (`WsMsg::Peers`,
//! with their addresses). With it a device that cannot find a peer by itself:
//! 1. tries the addresses it was told (another subnet, Tailscale ...): most "unreachable" peers are
//!    only undiscoverable (multicast filtered, different segment), and this links them directly;
//! 2. otherwise shows the peer online through the relay with the lowest round-trip time and sends
//!    its messages and files wrapped in `WsMsg::Relay` (binary pieces in a `relay_frame`). The
//!    relay forwards one hop only and only between peers it has links to itself.
//! A direct path always wins: the relay is used only while the peer is not online directly, and
//! the address attempts repeat (rate limited) on every gossip, so a path that opens is taken.
//! Nothing here needs the user: older peers ignore both messages.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::node::{fmt_sa, json_body, Node};
use crate::protocol::*;

/// Gossip goes out this often even without changes (it also expires nothing: a relay's hints
/// live as long as its link).
const GOSSIP_SECS: u64 = 45;
/// Address attempts for one peer at most this often.
const INTRO_EVERY: Duration = Duration::from_secs(60);
/// A relayed binary piece waits this long for room on the next hop before it is dropped (the
/// sender's chunk then times out and is resent).
const FORWARD_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct RelayHint {
    pub info: DeviceInfo,
    /// The relay's connection to the target (`PeerHint::link`).
    pub link: u64,
}

impl Node {
    // ------------------------------------------------------------------ gossip
    pub(crate) fn kick_gossip(&self) {
        self.gossip_kick.notify_one();
    }

    /// The peers we have a link to (except `for_peer`), with every address we know them by.
    fn gossip_snapshot(&self, for_peer: &str) -> Vec<PeerHint> {
        let ids: Vec<(String, u64)> = self.links.lock().unwrap().iter().filter(|(k, _)| k.as_str() != for_peer).map(|(k, l)| (k.clone(), l.conn_id)).collect();
        let peers = self.peers.read().unwrap();
        ids.into_iter()
            .filter(|(id, _)| !self.store.is_blocked(id))
            .filter_map(|(id, link)| {
                let p = peers.get(&id)?;
                let mut addrs = vec![p.addr.to_string()];
                for (a, _) in p.seen_at.iter().rev() {
                    let ll6 = matches!(a, IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80);
                    let s = a.to_string();
                    if !ll6 && !addrs.contains(&s) {
                        addrs.push(s);
                    }
                }
                Some(PeerHint { device: p.info.clone(), addrs, link })
            })
            .collect()
    }

    pub(crate) fn spawn_gossip(self: Arc<Self>) {
        let mut stop = self.shutdown_rx();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(GOSSIP_SECS)) => {}
                    _ = self.gossip_kick.notified() => {
                        // links come and go in bursts (start, network change): one round for all
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                    _ = stop.changed() => break,
                }
                let targets: Vec<(String, mpsc::UnboundedSender<String>)> = self.links.lock().unwrap().iter().map(|(k, l)| (k.clone(), l.tx.clone())).collect();
                for (id, tx) in targets {
                    let w = WsMsg::Peers { peers: self.gossip_snapshot(&id) };
                    let _ = tx.send(serde_json::to_string(&w).unwrap_or_default());
                }
            }
        });
    }

    /// `via` told us which peers it has links to.
    pub(crate) fn on_peers(self: &Arc<Self>, via: &str, hints: Vec<PeerHint>) {
        if self.store.is_blocked(via) {
            return;
        }
        let me = self.me().id;
        let hints: Vec<PeerHint> = hints.into_iter().filter(|h| !h.device.id.is_empty() && h.device.id != me && h.device.id != via && !self.store.is_blocked(&h.device.id)).collect();
        let before: HashSet<String> = self.relay_only_targets();
        let mut touched: HashSet<String> = HashSet::new();
        // targets whose link at `via` was re-made since the last gossip
        let mut remade: HashSet<String> = HashSet::new();
        {
            let mut r = self.relays.lock().unwrap();
            for h in &hints {
                let old = r.get(&h.device.id).and_then(|m| m.get(via)).map(|x| x.link);
                if h.link != 0 && old.is_some_and(|o| o != h.link) {
                    remade.insert(h.device.id.clone());
                }
            }
            for (t, m) in r.iter_mut() {
                if m.remove(via).is_some() {
                    touched.insert(t.clone());
                }
            }
            for h in &hints {
                r.entry(h.device.id.clone()).or_default().insert(via.to_string(), RelayHint { info: h.device.clone(), link: h.link });
                touched.insert(h.device.id.clone());
            }
            r.retain(|_, m| !m.is_empty());
        }
        for h in &hints {
            // a peer we never met: it appears in the list (as heard through the relay)
            match self.store.device(&h.device.id) {
                None => {
                    let _ = self.store.upsert_device(&h.device, "", now_ms());
                }
                Some(d) if d.name == "Unknown" || d.name.is_empty() => {
                    let _ = self.store.upsert_device(&h.device, &d.last_addr, now_ms());
                }
                _ => {}
            }
            if !self.is_online(&h.device.id) {
                let node = self.clone();
                let h = h.clone();
                tokio::spawn(async move { node.try_intro(h).await });
            }
        }
        let after = self.relay_only_targets();
        // the relay re-made its link to a peer we reach through it: what went through the old
        // one may be lost, send what is not acknowledged again (the receiver drops duplicates)
        for t in remade.iter().filter(|t| before.contains(*t) && after.contains(*t) && self.relay_via(t).as_deref() == Some(via)) {
            log::info!("{t}: relay link at {via} was re-made, resending");
            let node = self.clone();
            let t = t.clone();
            tokio::spawn(async move { node.flush_peer(&t).await });
        }
        for t in touched.iter().chain(before.iter()) {
            if before.contains(t) != after.contains(t) {
                self.emit_peer(t);
                if after.contains(t) {
                    log::info!("{t} reachable through a relay");
                    let node = self.clone();
                    let t = t.clone();
                    tokio::spawn(async move { node.flush_peer(&t).await });
                }
            }
        }
    }

    /// A link went away: forget what that peer relayed.
    pub(crate) fn relay_drop(&self, via: &str) {
        let before = self.relay_only_targets();
        {
            let mut r = self.relays.lock().unwrap();
            for m in r.values_mut() {
                m.remove(via);
            }
            r.retain(|_, m| !m.is_empty());
        }
        let after = self.relay_only_targets();
        for t in before.difference(&after) {
            self.emit_peer(t);
        }
    }

    /// Peers reachable only through a relay right now.
    fn relay_only_targets(&self) -> HashSet<String> {
        let ids: Vec<String> = self.relays.lock().unwrap().keys().cloned().collect();
        ids.into_iter().filter(|t| self.relay_via(t).is_some()).collect()
    }

    /// Tries the addresses a relay gave for `h`: a peer that answers there is simply online.
    async fn try_intro(self: Arc<Self>, h: PeerHint) {
        let id = h.device.id.clone();
        {
            let mut t = self.intro_tries.lock().unwrap();
            if t.get(&id).is_some_and(|at| at.elapsed() < INTRO_EVERY) {
                return;
            }
            t.insert(id.clone(), Instant::now());
        }
        let port = h.device.port;
        let me = self.me().id;
        let cands: Vec<IpAddr> = h.addrs.iter().filter_map(|a| a.parse::<IpAddr>().ok()).filter(|ip| self.is_trusted_source(ip)).take(8).collect();
        let tries = cands.iter().map(|ip| {
            let url = format!("http://{}/info", fmt_sa(&SocketAddr::new(*ip, port)));
            let req = self.http.get(url).header(HEADER_DEVICE_ID, me.clone()).timeout(Duration::from_secs(2)).send();
            let ip = *ip;
            async move {
                let r = req.await.ok()?.error_for_status().ok()?;
                let info: DeviceInfo = json_body(r).await.ok()?;
                Some((ip, info))
            }
        });
        let found = futures_util::future::join_all(tries).await.into_iter().flatten().find(|(_, info)| info.id == id);
        match found {
            Some((ip, info)) => {
                log::info!("{id}: reached directly at {ip} (address from a relay)");
                self.peer_seen(info, ip);
                // and let it know us, in case it cannot find us either
                self.introduce(&id).await;
            }
            None => log::debug!("{id}: none of {cands:?} answers, using a relay"),
        }
    }

    // ------------------------------------------------------------------ routing
    /// The relay to reach `target` through, when it is not online directly: the linked peer that
    /// has a link to it, lowest round-trip time first.
    pub(crate) fn relay_via(&self, target: &str) -> Option<String> {
        if self.is_online(target) || self.store.is_blocked(target) {
            return None;
        }
        let cands: Vec<String> = self.relays.lock().unwrap().get(target)?.keys().cloned().collect();
        let links = self.links.lock().unwrap();
        cands.into_iter().filter_map(|a| links.get(&a).map(|l| (l.rtt_ms.load(Ordering::Relaxed), a))).min().map(|(_, a)| a)
    }

    /// Online directly or through a relay.
    pub(crate) fn reachable(&self, id: &str) -> bool {
        self.is_online(id) || self.relay_via(id).is_some()
    }

    /// The relay's name, for a peer only reachable through one.
    pub(crate) fn relay_name(&self, target: &str) -> Option<String> {
        let via = self.relay_via(target)?;
        let name = self.peers.read().unwrap().get(&via).map(|p| p.info.name.clone());
        Some(name.unwrap_or(via))
    }

    /// Sends over the direct link, else through a relay. False when neither exists.
    pub(crate) fn send_to(&self, peer_id: &str, wire: &WsMsg) -> bool {
        if self.send_over_link(peer_id, wire) {
            return true;
        }
        let Some(via) = self.relay_via(peer_id) else { return false };
        let w = WsMsg::Relay { to: peer_id.to_string(), from: self.me().id, msg: serde_json::to_value(wire).unwrap_or_default() };
        self.send_over_link(&via, &w)
    }

    /// Where binary pieces for `peer_id` go: the direct link, or a relay (pieces then need
    /// `relay_frame`, signalled by `true`).
    pub(crate) fn data_path(&self, peer_id: &str) -> Option<(mpsc::Sender<Vec<u8>>, bool)> {
        if let Some(l) = self.links.lock().unwrap().get(peer_id) {
            return Some((l.data.clone(), false));
        }
        let via = self.relay_via(peer_id)?;
        self.links.lock().unwrap().get(&via).map(|l| (l.data.clone(), true))
    }

    /// A channel whose messages (serialized `WsMsg`) go to `peer_id` by whatever path exists:
    /// replies of the link-transfer handlers for a relayed sender.
    fn reply_tx(self: &Arc<Self>, peer_id: &str) -> mpsc::UnboundedSender<String> {
        let mut m = self.relay_replies.lock().unwrap();
        if let Some(tx) = m.get(peer_id).filter(|t| !t.is_closed()) {
            return tx.clone();
        }
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let node = self.clone();
        let id = peer_id.to_string();
        tokio::spawn(async move {
            while let Some(s) = rx.recv().await {
                if let Ok(w) = serde_json::from_str::<WsMsg>(&s) {
                    node.send_to(&id, &w);
                }
            }
        });
        m.insert(peer_id.to_string(), tx.clone());
        tx
    }

    // ------------------------------------------------------------------ relayed frames
    /// A `Relay` arrived over the link with `via`: forward it one hop, or handle it as from `from`.
    pub(crate) async fn on_relay(self: &Arc<Self>, via: &str, to: String, from: String, msg: serde_json::Value) {
        let me = self.me().id;
        if self.store.is_blocked(via) || self.store.is_blocked(&from) || from == me {
            return;
        }
        if to != me {
            // we are the relay: only between peers we have links to, one hop
            if from != via || self.store.is_blocked(&to) {
                return;
            }
            let w = WsMsg::Relay { to: to.clone(), from, msg };
            self.send_over_link(&to, &w);
            return;
        }
        let Ok(w) = serde_json::from_value::<WsMsg>(msg) else { return };
        match w {
            WsMsg::FileStart { manifest } => {
                let out = self.reply_tx(&from);
                self.link_file_start(&from, manifest, &out, true);
            }
            WsMsg::FileFinish { transfer_id } => {
                let out = self.reply_tx(&from);
                self.link_file_finish(&from, transfer_id, &out);
            }
            // link-level messages mean nothing end to end
            WsMsg::Hello { .. } | WsMsg::Peers { .. } | WsMsg::Relay { .. } | WsMsg::Ping | WsMsg::Pong | WsMsg::Bye | WsMsg::InboundBlocked { .. } => {}
            w => {
                if let Some(reply) = self.handle_wire(&from, w, None).await {
                    self.send_to(&from, &reply);
                }
            }
        }
    }

    /// A relayed binary piece. Returns the piece (as from whom) when it is ours to assemble.
    pub(crate) async fn on_relay_bin(self: &Arc<Self>, via: &str, frame: &[u8]) -> Option<(String, Vec<u8>, mpsc::UnboundedSender<String>)> {
        let (to, from, inner) = relay_unframe(frame)?;
        let me = self.me().id;
        if self.store.is_blocked(via) || self.store.is_blocked(&from) || from == me {
            return None;
        }
        if to != me {
            if from != via || self.store.is_blocked(&to) {
                return None;
            }
            let data = self.links.lock().unwrap().get(&to).map(|l| l.data.clone())?;
            let _ = tokio::time::timeout(FORWARD_WAIT, data.send(frame.to_vec())).await;
            return None;
        }
        let out = self.reply_tx(&from);
        Some((from, inner.to_vec(), out))
    }
}

/// Relay names keyed by peer id, for listing many peers at once (no locks held by the caller).
impl Node {
    pub(crate) fn relay_names(&self) -> HashMap<String, String> {
        let ids: Vec<String> = self.relays.lock().unwrap().keys().cloned().collect();
        ids.into_iter().filter_map(|t| self.relay_name(&t).map(|n| (t, n))).collect()
    }
}

/// Peers found through another peer: a direct path when the given address answers, otherwise
/// messages and files through the relay.
#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::{CoreEvent, Node, NodeConfig};

    async fn start(dir: &Path, name: &str, bind: Option<IpAddr>) -> Arc<Node> {
        let mut cfg = NodeConfig::new(dir.join("data"));
        cfg.port = 0;
        cfg.bind_ip = bind;
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
        cfg.name_override = Some(name.into());
        cfg.recv_dir_override = Some(dir.join("recv"));
        Node::start(cfg).await.expect("start node")
    }

    async fn until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
        tokio::time::timeout(Duration::from_secs(secs), async {
            while !f() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn address_from_gossip_gives_direct_path() {
        let (tx, ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let x = start(tx.path(), "x", None).await;
        let a = start(ta.path(), "a", None).await;
        let b = start(tb.path(), "b", None).await;
        x.add_peer_addr(&format!("127.0.0.1:{}", a.port())).await.unwrap();
        b.add_peer_addr(&format!("127.0.0.1:{}", a.port())).await.unwrap();
        let b_id = b.me().id;
        let x_id = x.me().id;
        until("x finds b directly", 15, || x.is_online(&b_id)).await;
        until("b finds x", 15, || b.is_online(&x_id)).await;
        let v = x.peers().into_iter().find(|p| p.id == b_id).unwrap();
        assert!(v.online && v.via.is_none(), "{v:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn text_and_file_through_relay() {
        let (tx, ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // x and b listen where nobody can reach them (they dial out from 127.0.0.1); a is reachable
        let x = start(tx.path(), "x", Some("127.0.0.3".parse().unwrap())).await;
        let a = start(ta.path(), "a", None).await;
        let b = start(tb.path(), "b", Some("127.0.0.2".parse().unwrap())).await;
        let (a_id, b_id, x_id) = (a.me().id, b.me().id, x.me().id);
        x.add_peer_addr(&format!("127.0.0.1:{}", a.port())).await.unwrap();
        b.add_peer_addr(&format!("127.0.0.1:{}", a.port())).await.unwrap();
        x.dial_link(&a_id, true).await;
        b.dial_link(&a_id, true).await;
        until("b online at x through a", 20, || x.peers().iter().any(|p| p.id == b_id && p.online && p.via.as_deref() == Some("a"))).await;
        until("x online at b through a", 20, || b.peers().iter().any(|p| p.id == x_id && p.online)).await;
        assert!(!x.is_online(&b_id), "no direct path should exist");

        let mut eb = b.subscribe();
        let mut ex = x.subscribe();
        let m = x.send_text(&b_id, "hello through a").await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(CoreEvent::Message { message }) = eb.recv().await {
                    if message.id == m.id {
                        return message;
                    }
                }
            }
        })
        .await
        .expect("b receives the relayed text");
        assert_eq!(got.peer_id, x_id);
        assert_eq!(got.body["text"], "hello through a");
        until("delivered ack back through a", 10, || x.messages(&b_id, None, 10).unwrap().iter().any(|mm| mm.id == m.id && mm.status == "delivered")).await;

        // a 9 MiB file (three chunks) through the relay
        let src = tx.path().join("relay.bin");
        let data: Vec<u8> = (0..9 * 1024 * 1024u32).map(|i| (i * 7 + 3) as u8).collect();
        std::fs::write(&src, &data).unwrap();
        let sent = x.send_files(&[b_id.clone()], &[src.clone()]).await.unwrap();
        let fid = sent[0].id.clone();
        tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                if let Ok(CoreEvent::MessageUpdate { message }) = ex.recv().await {
                    if message.id == fid && (message.status == "done" || message.status == "failed") {
                        assert_eq!(message.status, "done", "{:?}", message.body);
                        return;
                    }
                }
            }
        })
        .await
        .expect("file finishes through the relay");
        let got = std::fs::read(tb.path().join("recv").join("relay.bin")).unwrap();
        assert!(got == data, "file content differs");
    }

    /// A TCP forwarder: `cut` closes the far side of every connection and leaves the near side
    /// open and silent, like a path whose far end vanished (its network switched off).
    async fn blackhole(listen: std::net::SocketAddr, target: std::net::SocketAddr, src: IpAddr) -> tokio::sync::watch::Sender<bool> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind(listen).await.unwrap();
        let (cut_tx, cut_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            loop {
                let Ok((inc, _)) = l.accept().await else { return };
                if *cut_rx.borrow() {
                    continue; // dropped: refused
                }
                let sock = tokio::net::TcpSocket::new_v4().unwrap();
                sock.bind(std::net::SocketAddr::new(src, 0)).unwrap();
                let Ok(out) = sock.connect(target).await else { continue };
                let mut cut = cut_rx.clone();
                tokio::spawn(async move {
                    let (mut ir, mut iw) = inc.into_split();
                    let (mut or, mut ow) = out.into_split();
                    let (mut b1, mut b2) = (vec![0u8; 65536], vec![0u8; 65536]);
                    loop {
                        if *cut.borrow_and_update() {
                            break;
                        }
                        let step = tokio::select! {
                            _ = cut.changed() => 0,
                            r = ir.read(&mut b1) => match r { Ok(n) if n > 0 => { if ow.write_all(&b1[..n]).await.is_err() { return } 1 } _ => return },
                            r = or.read(&mut b2) => match r { Ok(n) if n > 0 => { if iw.write_all(&b2[..n]).await.is_err() { return } 2 } _ => return },
                        };
                        let _ = step;
                    }
                    drop((or, ow));
                    let _keep = (ir, iw);
                    std::future::pending::<()>().await;
                });
            }
        });
        cut_tx
    }

    /// The preferred link (dialed by the smaller id) dies silently on the other side; when that
    /// side dials again it must be let in at once, not rejected until the ping timeout (60–90 s),
    /// and what was sent into the dead link meanwhile arrives.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn silently_dead_link_is_replaced_when_the_peer_dials_again() {
        let (t1, t2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let ip1: IpAddr = "127.0.0.2".parse().unwrap();
        let ip2: IpAddr = "127.0.0.3".parse().unwrap();
        let n1 = start(t1.path(), "n1", Some(ip1)).await;
        let n2 = start(t2.path(), "n2", Some(ip2)).await;
        let ((small, small_ip), (large, large_ip)) = if n1.me().id < n2.me().id { ((n1, ip1), (n2, ip2)) } else { ((n2, ip2), (n1, ip1)) };
        let (sid, lid) = (small.me().id, large.me().id);
        let via: std::net::SocketAddr = format!("127.0.0.5:{}", large.port()).parse().unwrap();
        let cut = blackhole(via, std::net::SocketAddr::new(large_ip, large.port()), small_ip).await;
        small.add_peer_addr(&via.to_string()).await.unwrap();
        until("linked through the forwarder", 15, || small.links.lock().unwrap().contains_key(&lid) && large.links.lock().unwrap().contains_key(&sid)).await;

        cut.send(true).unwrap();
        until("large side notices", 10, || !large.links.lock().unwrap().contains_key(&sid)).await;
        assert!(small.links.lock().unwrap().contains_key(&lid), "small side still holds the dead link");

        let mut el = large.subscribe();
        let m = small.send_text(&lid, "after the path died").await.unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Ok(CoreEvent::Message { message }) = el.recv().await {
                    if message.id == m.id {
                        return;
                    }
                }
            }
        })
        .await
        .expect("message arrives over the new link well before the ping timeout");
    }
}
