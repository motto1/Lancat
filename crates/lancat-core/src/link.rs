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

    async fn dial(self: Arc<Self>, peer_id: String) -> anyhow::Result<()> {
        let (ip, port) = self.peer_addr(&peer_id).ok_or_else(|| anyhow::anyhow!("no address"))?;
        let url = format!("ws://{}/ws", fmt_sa(&SocketAddr::new(ip, port)));
        let mut req = url.into_client_request()?;
        req.headers_mut().insert(HEADER_DEVICE_ID, self.me().id.parse()?);
        let (ws, _) = tokio::time::timeout(Duration::from_secs(5), tokio_tungstenite::connect_async(req)).await??;
        let (mut sink, mut stream) = ws.split();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(s) = out_rx.recv().await {
                if sink.send(TMsg::Text(s.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        tokio::spawn(async move {
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
        tokio::spawn(self.run_link(Some(peer_id), ip, in_rx, out_tx, true));
        Ok(())
    }

    /// Accept side, called from the axum WebSocket upgrade.
    pub(crate) async fn accept_ws(self: Arc<Self>, socket: axum::extract::ws::WebSocket, ip: IpAddr) {
        use axum::extract::ws::Message as AMsg;
        let (mut sink, mut stream) = socket.split();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<String>();
        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(s) = out_rx.recv().await {
                if sink.send(AMsg::Text(s.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        tokio::spawn(async move {
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
        if device.id == self.me().id || expected.as_ref().is_some_and(|e| *e != device.id) || self.store.is_blocked(&device.id) {
            return;
        }
        if !dialer && out_tx.send(hello).is_err() {
            return;
        }
        let peer_id = device.id.clone();
        self.peer_seen(device, ip);
        let conn_id = self.conn_seq.fetch_add(1, Ordering::Relaxed);
        self.links.lock().unwrap().insert(peer_id.clone(), Link { tx: out_tx.clone(), conn_id });
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
    }
}
