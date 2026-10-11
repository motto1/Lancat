//! Per-peer WebSocket links. One long-lived connection per pair of devices:
//! the device with the lexicographically smaller id dials; the other accepts.
//! Messages also have an HTTP fallback (`POST /msg`), so a missing link never loses data.
//!
//! The link is also the liveness signal: both ends ping every 30 s; a link that hears nothing for
//! 2 intervals is dropped and the peer marked offline (60–90 s after it went silent). A closed
//! connection triggers an immediate re-probe; `bye` marks the peer offline at once.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as TMsg;

use crate::node::{fmt_sa, Link, Node};
use crate::protocol::*;

impl Node {
    /// Dials the peer if we are the designated dialer and no link exists.
    pub(crate) async fn ensure_link(self: &Arc<Self>, peer_id: &str) {
        self.dial_link(peer_id, false).await
    }

    /// `force`: dial even though the peer is the designated dialer (it has not managed to reach us).
    pub(crate) async fn dial_link(self: &Arc<Self>, peer_id: &str, force: bool) {
        let me = self.me();
        if (!force && me.id.as_str() >= peer_id) || !self.is_online(peer_id) || self.store.is_blocked(peer_id) || self.is_stopped() {
            return;
        }
        if self.links.lock().unwrap().contains_key(peer_id) {
            return;
        }
        if !self.dialing.lock().unwrap().insert(peer_id.to_string()) {
            return;
        }
        // on success the entry is cleared by run_link once the handshake finished
        if let Err(e) = self.clone().dial(peer_id.to_string()).await {
            self.dialing.lock().unwrap().remove(peer_id);
            log::debug!("dial {peer_id} failed: {e}");
        }
    }

    /// Dials a second link at `ip` although one exists (moving it to a better path); the new link
    /// replaces the old one when its handshake completes.
    pub(crate) async fn dial_link_at(self: &Arc<Self>, peer_id: &str, ip: IpAddr) {
        if !self.dialing.lock().unwrap().insert(peer_id.to_string()) {
            return;
        }
        if let Err(e) = self.clone().dial_to(peer_id.to_string(), Some(ip)).await {
            self.dialing.lock().unwrap().remove(peer_id);
            log::debug!("dial {peer_id} at {ip} failed: {e}");
        }
    }

    async fn dial(self: Arc<Self>, peer_id: String) -> anyhow::Result<()> {
        self.dial_to(peer_id, None).await
    }

    async fn dial_to(self: Arc<Self>, peer_id: String, at: Option<IpAddr>) -> anyhow::Result<()> {
        let (ip, port) = self.peer_addr(&peer_id).ok_or_else(|| anyhow::anyhow!("no address"))?;
        let ip = at.unwrap_or(ip);
        let url = format!("ws://{}/ws", fmt_sa(&SocketAddr::new(ip, port)));
        let mut req = url.into_client_request()?;
        req.headers_mut().insert(HEADER_DEVICE_ID, self.me().id.parse()?);
        // disable_nagle: small frames (text, typing, acks, pings) go out at once instead of
        // waiting up to ~40 ms for Nagle/delayed-ACK to coalesce them
        let (ws, _) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async_with_config(req, None, true)).await??;
        let (mut sink, mut stream) = ws.split();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(s) = out_rx.recv().await {
                if sink.send(TMsg::Text(s.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let reader = tokio::spawn(async move {
            while let Some(Ok(m)) = stream.next().await {
                match m {
                    TMsg::Text(t) => {
                        if in_tx.send(t.as_str().to_string()).is_err() {
                            break;
                        }
                    }
                    TMsg::Close(_) => break,
                    _ => {}
                }
            }
        });
        tokio::spawn(async move {
            self.run_link(Some(peer_id), ip, in_rx, out_tx, true).await;
            end_io(reader, writer).await;
        });
        Ok(())
    }

    /// Accept side, called from the axum WebSocket upgrade.
    pub(crate) async fn accept_ws(self: Arc<Self>, socket: axum::extract::ws::WebSocket, ip: IpAddr) {
        use axum::extract::ws::Message as AMsg;
        let (mut sink, mut stream) = socket.split();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        let writer = tokio::spawn(async move {
            while let Some(s) = out_rx.recv().await {
                if sink.send(AMsg::Text(s.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let reader = tokio::spawn(async move {
            while let Some(Ok(m)) = stream.next().await {
                match m {
                    AMsg::Text(t) => {
                        if in_tx.send(t.as_str().to_string()).is_err() {
                            break;
                        }
                    }
                    AMsg::Close(_) => break,
                    _ => {}
                }
            }
        });
        self.run_link(None, ip, in_rx, out_tx, false).await;
        end_io(reader, writer).await;
    }

    /// Transport-independent link loop: hello handshake, registration, keepalive, dispatch.
    pub(crate) async fn run_link(
        self: Arc<Self>,
        expected: Option<String>,
        ip: IpAddr,
        mut in_rx: mpsc::UnboundedReceiver<String>,
        out_tx: mpsc::UnboundedSender<String>,
        dialer: bool,
    ) {
        // whatever happens below, a dial attempt to `expected` is over when this returns or registers
        struct DialGuard<'a>(&'a Node, Option<String>);
        impl Drop for DialGuard<'_> {
            fn drop(&mut self) {
                if let Some(id) = self.1.take() {
                    self.0.dialing.lock().unwrap().remove(&id);
                }
            }
        }
        let guard = DialGuard(&self, if dialer { expected.clone() } else { None });
        let me_id = self.me().id;
        let hello = serde_json::to_string(&WsMsg::Hello { device: self.me() }).unwrap_or_default();
        if dialer && out_tx.send(hello.clone()).is_err() {
            return;
        }
        let device = match tokio::time::timeout(Duration::from_secs(5), in_rx.recv()).await {
            Ok(Some(t)) => match serde_json::from_str::<WsMsg>(&t) {
                Ok(WsMsg::Hello { device }) => device,
                _ => return,
            },
            _ => return,
        };
        if device.id == me_id || expected.as_ref().is_some_and(|e| *e != device.id) || self.store.is_blocked(&device.id) {
            return;
        }
        if !dialer && out_tx.send(hello).is_err() {
            return;
        }
        let peer_id = device.id.clone();
        // Simultaneous dials (both ends dialing at once: a forced dial from the non-designated
        // side, a path upgrade racing a reconnect) used to end with each side keeping a different
        // connection — the newest it registered — and closing the other, i.e. the one its peer
        // kept: both links died and the pair reconnected over and over. Both ends now apply the
        // same rule: the connection dialed by the smaller device id wins; between two equally
        // ranked ones the newer replaces the older (path upgrades).
        self.peer_seen(device, ip);
        let preferred = if dialer { me_id.as_str() < peer_id.as_str() } else { peer_id.as_str() < me_id.as_str() };
        let conn_id = self.conn_seq.fetch_add(1, Ordering::Relaxed);
        {
            let mut links = self.links.lock().unwrap();
            if links.get(&peer_id).is_some_and(|l| l.preferred && !preferred) {
                log::debug!("link with {peer_id}: keeping the connection dialed by the smaller id, dropping this one");
                return;
            }
            links.insert(peer_id.clone(), Link { tx: out_tx.clone(), conn_id, preferred });
        }
        // the address a working link runs over is the one to dial next time
        self.set_peer_addr(&peer_id, ip);
        drop(guard); // dial finished: allow future reconnects
        log::info!("link up with {peer_id} ({})", if dialer { "dialed" } else { "accepted" });
        {
            let node = self.clone();
            let id = peer_id.clone();
            tokio::spawn(async move { node.flush_peer(&id).await });
        }

        let ping = serde_json::to_string(&WsMsg::Ping).unwrap_or_default();
        let mut tick = tokio::time::interval(Duration::from_secs(LINK_PING_SECS));
        // after a freeze/suspend only one tick fires, so the peer gets a chance to answer
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        let mut stop = self.shutdown_rx();
        let mut idle = 0u32;
        // why the loop ended: decides whether the peer goes offline
        let mut timed_out = false;
        let mut closed = false;
        loop {
            tokio::select! {
                m = in_rx.recv() => {
                    let Some(t) = m else { closed = true; break };
                    idle = 0;
                    if let Ok(w) = serde_json::from_str::<WsMsg>(&t) {
                        if let Some(reply) = self.handle_wire(&peer_id, w, Some(ip)).await {
                            let _ = out_tx.send(serde_json::to_string(&reply).unwrap_or_default());
                        }
                    }
                }
                _ = tick.tick() => {
                    idle += 1;
                    if idle > LINK_MISSED_PINGS { timed_out = true; break; }
                    if out_tx.send(ping.clone()).is_err() { closed = true; break; }
                }
                _ = stop.changed() => break,
            }
            let still_ours = self.links.lock().unwrap().get(&peer_id).map(|l| l.conn_id) == Some(conn_id);
            if !still_ours {
                break;
            }
        }
        let ours = {
            let mut links = self.links.lock().unwrap();
            let ours = links.get(&peer_id).map(|l| l.conn_id) == Some(conn_id);
            if ours {
                links.remove(&peer_id);
            }
            ours
        };
        log::info!("link down with {peer_id}{}", if timed_out { " (ping timeout)" } else if closed { " (closed)" } else { "" });
        // superseded by a newer link, bye, or our own shutdown: nothing to do
        if ours && (timed_out || closed) {
            self.link_lost(&peer_id, timed_out);
        }
        if ours {
            self.kick_housekeeping();
        }
    }
}

/// A link loop ended: stop its socket tasks. The reader used to outlive the link until the remote
/// closed the socket, forever after a ping timeout (peer gone, no FIN): one task + socket leaked
/// per lost link. The writer gets a moment to flush what is queued (bye, last acks) and close.
async fn end_io(reader: tokio::task::JoinHandle<()>, writer: tokio::task::JoinHandle<()>) {
    reader.abort();
    let w = writer.abort_handle();
    if tokio::time::timeout(Duration::from_secs(3), writer).await.is_err() {
        w.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::{Node, NodeConfig};

    async fn start(dir: &std::path::Path, name: &str) -> Arc<Node> {
        let mut cfg = NodeConfig::new(dir.join("data"));
        cfg.port = 0;
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
        cfg.name_override = Some(name.into());
        cfg.recv_dir_override = Some(dir.join("recv"));
        Node::start(cfg).await.expect("start node")
    }

    /// Both ends dial each other at the same moment (twice over): afterwards they must agree on
    /// one connection, i.e. the link stays up without a gap and without reconnecting.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn simultaneous_dials_converge() {
        let (ta, tb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a = start(ta.path(), "alpha").await;
        let b = start(tb.path(), "bravo").await;
        a.add_peer_addr(&format!("127.0.0.1:{}", b.port())).await.unwrap();
        let (aid, bid) = (a.me().id, b.me().id);
        let linked = |n: &Arc<Node>, id: &str| n.links.lock().unwrap().contains_key(id);
        tokio::time::timeout(Duration::from_secs(10), async {
            while !(linked(&a, &bid) && linked(&b, &aid)) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("initial link");
        let lo: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        for _ in 0..2 {
            let (a2, b2, aid2, bid2) = (a.clone(), b.clone(), aid.clone(), bid.clone());
            tokio::join!(a2.dial_link_at(&bid2, lo), b2.dial_link_at(&aid2, lo));
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        let ids = |n: &Arc<Node>, id: &str| n.links.lock().unwrap().get(id).map(|l| l.conn_id);
        let (ca, cb) = (ids(&a, &bid), ids(&b, &aid));
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(linked(&a, &bid) && linked(&b, &aid), "link dropped after simultaneous dials");
        }
        assert_eq!((ca, cb), (ids(&a, &bid), ids(&b, &aid)), "links kept changing");
        a.shutdown();
        b.shutdown();
    }
}
