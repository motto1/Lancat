//! Per-peer WebSocket links. One long-lived connection per pair of devices:
//! the device with the lexicographically smaller id dials; the other accepts.
//! Messages also have an HTTP fallback (`POST /msg`), so a missing link never loses data.

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
        let me = self.me();
        if me.id.as_str() >= peer_id || !self.is_online(peer_id) || self.store.is_blocked(peer_id) {
            return;
        }
        if self.links.lock().unwrap().contains_key(peer_id) {
            return;
        }
        if !self.dialing.lock().unwrap().insert(peer_id.to_string()) {
            return;
        }
        let res = self.clone().dial(peer_id.to_string()).await;
        self.dialing.lock().unwrap().remove(peer_id);
        if let Err(e) = res {
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
        log::info!("link up with {peer_id} ({})", if dialer { "dialed" } else { "accepted" });
        {
            let node = self.clone();
            let id = peer_id.clone();
            tokio::spawn(async move { node.flush_peer(&id).await });
        }

        let ping = serde_json::to_string(&WsMsg::Ping).unwrap_or_default();
        let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
        tick.tick().await;
        let mut stop = self.shutdown_rx();
        let mut idle = 0u32;
        loop {
            tokio::select! {
                m = in_rx.recv() => {
                    let Some(t) = m else { break };
                    idle = 0;
                    if let Ok(w) = serde_json::from_str::<WsMsg>(&t) {
                        if let Some(reply) = self.handle_wire(&peer_id, w, Some(ip)).await {
                            let _ = out_tx.send(serde_json::to_string(&reply).unwrap_or_default());
                        }
                    }
                }
                _ = tick.tick() => {
                    idle += 1;
                    if idle > 3 || out_tx.send(ping.clone()).is_err() { break; }
                }
                _ = stop.changed() => break,
            }
            let still_ours = self.links.lock().unwrap().get(&peer_id).map(|l| l.conn_id) == Some(conn_id);
            if !still_ours {
                break;
            }
        }
        let mut links = self.links.lock().unwrap();
        if links.get(&peer_id).map(|l| l.conn_id) == Some(conn_id) {
            links.remove(&peer_id);
        }
        log::info!("link down with {peer_id}");
    }
}
