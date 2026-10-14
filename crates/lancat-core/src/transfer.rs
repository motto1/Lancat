//! Outgoing file transfers: 4 MiB chunks over HTTP PUT, 4 in flight, resumable, BLAKE3-verified.

use std::fs::File;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::{stream, TryStreamExt};

use crate::errors::{classify, terr, ErrCode};
use crate::events::CoreEvent;
use crate::files::{discard_partial, expand_paths, hash_file, read_at};
use crate::netutil;
use crate::node::{fmt_sa, json_body, Node};
use crate::protocol::*;
use crate::store::{FileBody, Message};

pub const PARALLEL_CHUNKS: usize = 4;
/// Outgoing transfers that move data at the same time; the rest wait (still pausable/cancellable).
/// Bounds connections and memory: at most this × PARALLEL_CHUNKS chunks (4 MiB each) in flight,
/// however many files a folder drop queued.
pub const MAX_ACTIVE_TRANSFERS: usize = 3;

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;
const PEER_CANCEL: u8 = 3;

/// Stop switch of one running outgoing transfer. Stopping drops the transfer future, which aborts
/// the requests in flight; the receiver keeps what it has, so a paused transfer resumes from there.
#[derive(Default)]
pub(crate) struct TransferCtl {
    state: AtomicU8,
    notify: tokio::sync::Notify,
}

impl TransferCtl {
    fn stop(&self, how: u8) {
        self.state.store(how, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn get(&self) -> u8 {
        self.state.load(Ordering::SeqCst)
    }

    async fn stopped(&self) {
        loop {
            let n = self.notify.notified();
            tokio::pin!(n);
            n.as_mut().enable();
            if self.get() != RUN {
                return;
            }
            n.await;
        }
    }
}

/// The receiver answered 410: it cancelled this transfer.
#[derive(Debug)]
struct PeerCancelled;

impl std::fmt::Display for PeerCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("对方已取消")
    }
}

impl std::error::Error for PeerCancelled {}

impl Node {
    /// Sends files and folders (recursively) to each peer. Returns the created messages.
    pub async fn send_files(self: &Arc<Self>, peer_ids: &[String], paths: &[PathBuf]) -> Result<Vec<Message>> {
        let paths = paths.to_vec();
        let files = tokio::task::spawn_blocking(move || expand_paths(&paths)).await?;
        if files.is_empty() {
            bail!("没有可发送的文件");
        }
        let mut out = vec![];
        for peer in peer_ids {
            for (path, name) in &files {
                let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                let body = FileBody {
                    name: name.clone(),
                    size,
                    transfer_id: uuid::Uuid::new_v4().to_string(),
                    path: path.to_string_lossy().to_string(),
                    hash: String::new(),
                    ..Default::default()
                };
                let m = Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    peer_id: peer.clone(),
                    outgoing: true,
                    kind: "file".into(),
                    body: serde_json::to_value(&body)?,
                    status: "transferring".into(),
                    ts: crate::protocol::msg_stamp(),
                };
                self.store.insert_message(&m, false)?;
                self.emit(CoreEvent::Message { message: m.clone() });
                out.push(m);
            }
        }
        for m in &out {
            let node = self.clone();
            let id = m.id.clone();
            tokio::spawn(async move { node.run_transfer(id).await });
        }
        Ok(out)
    }

    /// Retries a failed outgoing transfer (resumes where the receiver left off).
    pub async fn retry_transfer(self: &Arc<Self>, msg_id: &str) -> Result<()> {
        let m = self.store.message(msg_id)?.ok_or_else(|| anyhow!("message gone"))?;
        if m.status == "paused" {
            self.set_status(msg_id, "transferring");
        }
        let node = self.clone();
        let id = msg_id.to_string();
        tokio::spawn(async move { node.run_transfer(id).await });
        Ok(())
    }

    /// Runs (or resumes) one outgoing transfer; never runs twice concurrently for the same message.
    pub(crate) async fn run_transfer(self: Arc<Self>, msg_id: String) {
        if !self.active_out.lock().unwrap().insert(msg_id.clone()) {
            return;
        }
        let ctl = Arc::new(TransferCtl::default());
        {
            // under the lock: a pause/cancel either sees this switch or already parked the message
            let mut ctls = self.out_ctl.lock().unwrap();
            let parked = !matches!(self.store.message(&msg_id), Ok(Some(m)) if matches!(m.status.as_str(), "transferring" | "failed" | "pending"));
            if parked {
                drop(ctls);
                self.active_out.lock().unwrap().remove(&msg_id);
                return;
            }
            ctls.insert(msg_id.clone(), ctl.clone());
        }
        let res = tokio::select! {
            r = async {
                // queued behind MAX_ACTIVE_TRANSFERS others; a pause/cancel meanwhile still lands
                let _slot = self.transfer_slots.acquire().await.map_err(|_| anyhow!("shutting down"))?;
                self.transfer_inner(&msg_id).await
            } => r,
            _ = ctl.stopped() => Err(anyhow!("stopped")),
        };
        self.out_ctl.lock().unwrap().remove(&msg_id);
        self.active_out.lock().unwrap().remove(&msg_id);
        let Ok(Some(m)) = self.store.message(&msg_id) else { return };
        let mut body: FileBody = serde_json::from_value(m.body.clone()).unwrap_or_default();
        let (status, error, code) = match res {
            Ok(()) => ("done", None, None),
            Err(_) if ctl.get() == PAUSE => {
                log::info!("transfer {msg_id} paused");
                ("paused", None, None)
            }
            Err(_) if ctl.get() == CANCEL => {
                log::info!("transfer {msg_id} cancelled");
                self.send_to(&m.peer_id, &WsMsg::FileCancel { transfer_id: body.transfer_id.clone() });
                ("cancelled", None, None)
            }
            Err(e) if ctl.get() == PEER_CANCEL || e.downcast_ref::<PeerCancelled>().is_some() => {
                log::info!("transfer {msg_id} cancelled by the receiver");
                ("cancelled", Some(PeerCancelled.to_string()), None)
            }
            Err(e) => {
                let code = classify(&e);
                log::warn!("transfer {msg_id} failed ({}): {e:#}", code.as_str());
                ("failed", Some(e.to_string()), Some(code.as_str().to_string()))
            }
        };
        body.error = error;
        body.code = code;
        if let Ok(Some(m)) = self.store.update_message(&msg_id, status, Some(&serde_json::to_value(&body).unwrap_or_default())) {
            self.emit(CoreEvent::MessageUpdate { message: m });
        }
    }

    /// Pauses an outgoing transfer. A paused one is not resumed on its own (reconnect, restart);
    /// `retry_transfer` continues it where the receiver left off.
    pub fn pause_transfer(&self, msg_id: &str) -> Result<()> {
        let ctls = self.out_ctl.lock().unwrap();
        if let Some(c) = ctls.get(msg_id) {
            c.stop(PAUSE);
            return Ok(());
        }
        // not running right now (peer offline, failed): park it so it is not picked up again
        let m = self.store.message(msg_id)?.ok_or_else(|| anyhow!("message gone"))?;
        if m.outgoing && m.kind == "file" && matches!(m.status.as_str(), "transferring" | "failed" | "pending") {
            self.set_status(msg_id, "paused");
        }
        Ok(())
    }

    /// Cancels a transfer in either direction and tells the other side.
    pub fn cancel_transfer(&self, msg_id: &str) -> Result<()> {
        let m = self.store.message(msg_id)?.ok_or_else(|| anyhow!("message gone"))?;
        if m.kind != "file" || matches!(m.status.as_str(), "done" | "cancelled") {
            return Ok(());
        }
        let body: FileBody = serde_json::from_value(m.body.clone()).unwrap_or_default();
        let ctls = self.out_ctl.lock().unwrap();
        if m.outgoing {
            if let Some(c) = ctls.get(msg_id) {
                c.stop(CANCEL); // run_transfer records it and tells the receiver
                return Ok(());
            }
        } else {
            self.drop_incoming(&body.transfer_id);
        }
        self.set_status(msg_id, "cancelled");
        drop(ctls);
        self.send_to(&m.peer_id, &WsMsg::FileCancel { transfer_id: body.transfer_id });
        Ok(())
    }

    /// The other side cancelled `transfer_id`.
    pub(crate) fn peer_cancelled(&self, peer_id: &str, transfer_id: &str) {
        let Ok(Some(m)) = self.store.message_by_transfer(peer_id, transfer_id) else { return };
        if matches!(m.status.as_str(), "done" | "cancelled") {
            return;
        }
        if m.outgoing {
            if let Some(c) = self.out_ctl.lock().unwrap().get(&m.id) {
                c.stop(PEER_CANCEL);
                return;
            }
            let mut body: FileBody = serde_json::from_value(m.body.clone()).unwrap_or_default();
            body.error = Some(PeerCancelled.to_string());
            if let Ok(Some(m)) = self.store.update_message(&m.id, "cancelled", Some(&serde_json::to_value(&body).unwrap_or_default())) {
                self.emit(CoreEvent::MessageUpdate { message: m });
            }
        } else {
            self.drop_incoming(transfer_id);
            self.set_status(&m.id, "cancelled");
        }
    }

    /// Forgets an incoming transfer and deletes its partial data (retried while chunk writes that
    /// are still in flight keep the part file open).
    fn drop_incoming(&self, transfer_id: &str) {
        // Close the sidecar gate first: a chunk write still in flight would otherwise re-create
        // the progress file after it was deleted (left behind in .lancat-part for good).
        let gate = self.incoming.lock().unwrap().remove(transfer_id).map(|t| {
            t.gate.close();
            t.gate.clone()
        });
        let dir = self.receive_dir();
        let tid = transfer_id.to_string();
        self.rt.spawn(async move {
            for _ in 0..20 {
                let done = match &gate {
                    Some(g) => g.exclusive(|| discard_partial(&dir, &tid)),
                    None => discard_partial(&dir, &tid),
                };
                if done {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            log::warn!("could not delete the partial data of transfer {tid}");
        });
    }

    async fn transfer_inner(self: &Arc<Self>, msg_id: &str) -> Result<()> {
        let m = self.store.message(msg_id)?.ok_or_else(|| anyhow!("message gone"))?;
        let mut body: FileBody = serde_json::from_value(m.body.clone())?;
        let path = PathBuf::from(&body.path);
        if !path.is_file() {
            return Err(terr(ErrCode::SourceMissing, "源文件不存在"));
        }
        if m.status != "transferring" {
            if let Ok(Some(m)) = self.store.update_message(msg_id, "transferring", None) {
                self.emit(CoreEvent::MessageUpdate { message: m });
            }
        }
        let size = std::fs::metadata(&path)?.len();
        if body.hash.is_empty() || size != body.size {
            let p = path.clone();
            body.hash = tokio::task::spawn_blocking(move || hash_file(&p)).await??;
            body.size = size;
            self.store.update_message(msg_id, "transferring", Some(&serde_json::to_value(&body)?))?;
        }
        if !self.reachable(&m.peer_id) {
            return Err(terr(ErrCode::PeerOffline, "对方不在线"));
        }
        let route = self.pick_route(&m.peer_id).await?;
        let me = self.me().id;

        // announce, so the receiver's UI shows it right away
        let offer = WsMsg::FileOffer { id: m.id.clone(), transfer_id: body.transfer_id.clone(), name: body.name.clone(), size, ts: m.ts };
        self.send_to(&m.peer_id, &offer);

        let manifest = FileManifest {
            transfer_id: body.transfer_id.clone(),
            msg_id: m.id.clone(),
            from: me.clone(),
            name: body.name.clone(),
            size,
            chunk_size: CHUNK_SIZE,
            hash: body.hash.clone(),
            ts: m.ts,
        };
        let base = match route {
            Route::Http(base) => base,
            Route::Link => return self.link_transfer(&m, &manifest, &path).await,
        };
        for attempt in 0..2 {
            let resp = self
                .http
                .post(format!("{base}/files/{}", manifest.transfer_id))
                .header(HEADER_DEVICE_ID, &me)
                .header("content-type", "application/json")
                .body(serde_json::to_vec(&manifest)?)
                .timeout(Duration::from_secs(15))
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::GONE {
                return Err(PeerCancelled.into());
            }
            if !resp.status().is_success() {
                let code = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(terr(ErrCode::from_peer(Some(code.as_u16()), None), format!("对方拒绝: {code} {text}")));
            }
            let status: TransferStatus = json_body(resp).await?;
            let have: std::collections::HashSet<u64> = status.have.into_iter().collect();
            let total_chunks = manifest.chunk_count();
            let missing: Vec<u64> = (0..total_chunks).filter(|i| !have.contains(i)).collect();
            let done_bytes = Arc::new(AtomicU64::new(have.iter().map(|&i| chunk_len(size, i)).sum()));
            let meter = self.meter(&manifest.transfer_id, &m.id, &m.peer_id, size, true);
            meter.emit(self, done_bytes.load(Ordering::Relaxed), true);

            let file = Arc::new(File::open(&path)?);
            // try_for_each_concurrent: the first chunk that fails for good ends the transfer at
            // once and drops (aborts) the requests still in flight, instead of pushing every
            // remaining chunk (each with 3 tries and a 60 s timeout) before reporting the error.
            stream::iter(missing.into_iter().map(Ok::<u64, anyhow::Error>))
                .try_for_each_concurrent(PARALLEL_CHUNKS, |idx| {
                    let node = self.clone();
                    let file = file.clone();
                    let base = base.clone();
                    let me = me.clone();
                    let tid = manifest.transfer_id.clone();
                    let done_bytes = done_bytes.clone();
                    let msg = m.clone();
                    let meter = meter.clone();
                    async move {
                        let len = chunk_len(size, idx) as usize;
                        let data = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                            let mut buf = vec![0u8; len];
                            read_at(&file, &mut buf, idx * CHUNK_SIZE)?;
                            Ok(buf)
                        })
                        .await??;
                        let hash = blake3::hash(&data).to_hex().to_string();
                        let data = axum::body::Bytes::from(data);
                        let mut last_err = None;
                        for _ in 0..3 {
                            let sent = Arc::new(AtomicU64::new(0));
                            let body = crate::progress::counting_body(data.clone(), meter.clone(), node.clone(), done_bytes.clone(), sent.clone());
                            let r = node
                                .http
                                .put(format!("{base}/files/{tid}/{idx}"))
                                .header(HEADER_DEVICE_ID, &me)
                                .header(HEADER_CHUNK_HASH, &hash)
                                .header(reqwest::header::CONTENT_LENGTH, len)
                                .body(body)
                                .timeout(Duration::from_secs(60))
                                .send()
                                .await;
                            // confirmed below on success; a failed attempt takes its bytes back
                            meter.add(-(sent.swap(0, Ordering::Relaxed) as i64));
                            match r {
                                Ok(r) if r.status().is_success() => {
                                    last_err = None;
                                    break;
                                }
                                Ok(r) if r.status() == reqwest::StatusCode::GONE => return Err(PeerCancelled.into()),
                                // the receiver cannot store it: retrying will not help
                                Ok(r) if r.status() == reqwest::StatusCode::INSUFFICIENT_STORAGE => {
                                    let text = r.text().await.unwrap_or_default();
                                    return Err(terr(ErrCode::PeerDirUnwritable, format!("对方无法保存文件: {text}")));
                                }
                                Ok(r) => last_err = Some(terr(ErrCode::PeerRejected, format!("chunk {idx}: HTTP {}", r.status()))),
                                Err(e) => last_err = Some(terr(ErrCode::Network, format!("chunk {idx}: {e}"))),
                            }
                            tokio::time::sleep(Duration::from_millis(300)).await;
                        }
                        if let Some(e) = last_err {
                            return Err(e);
                        }
                        let done = done_bytes.fetch_add(len as u64, Ordering::Relaxed) + len as u64;
                        node.touch_peer(&msg.peer_id);
                        meter.emit(&node, done, true);
                        Ok(())
                    }
                })
                .await?;

            let r = self
                .http
                .post(format!("{base}/finish/{}", manifest.transfer_id))
                .header(HEADER_DEVICE_ID, &me)
                .timeout(Duration::from_secs(120))
                .send()
                .await?;
            if r.status().is_success() {
                self.meter_drop(&manifest.transfer_id);
                return Ok(());
            }
            if r.status() == reqwest::StatusCode::CONFLICT && attempt == 0 {
                log::warn!("hash mismatch on {}, restarting transfer", manifest.name);
                continue;
            }
            let code = r.status();
            let text = r.text().await.unwrap_or_default();
            let ec = if code == reqwest::StatusCode::CONFLICT { ErrCode::Verify } else { ErrCode::from_peer(Some(code.as_u16()), None) };
            return Err(terr(ec, format!("完成失败: {code} {text}")));
        }
        Err(terr(ErrCode::Verify, "校验失败"))
    }
}

/// How an outgoing transfer reaches the receiver.
enum Route {
    /// HTTP chunk uploads to this base URL (the receiver accepts connections): the fast path.
    Http(String),
    /// Over the link the receiver dialed (it cannot be reached inbound).
    Link,
}

impl Node {
    /// HTTP when the receiver answers on its address; else the link, which works whenever either
    /// side could connect (texts arrive over it too); else, only with `alt_addr_fallback`, another
    /// address. A receiver whose firewall drops inbound connections still dials us, so its link is
    /// the one path that is sure to exist.
    async fn pick_route(&self, peer_id: &str) -> Result<Route> {
        // no direct path at all, but a relay: the link transfer runs through it
        if !self.is_online(peer_id) && self.relay_via(peer_id).is_some() {
            return Ok(Route::Link);
        }
        let (ip, port) = self.peer_addr(peer_id).ok_or_else(|| terr(ErrCode::PeerOffline, "对方地址未知"))?;
        let linked = || self.links.lock().unwrap().contains_key(peer_id);
        let known_blocked = matches!(self.http_unreachable.lock().unwrap().get(peer_id), Some((a, t)) if *a == ip && t.elapsed() < Duration::from_secs(60));
        if known_blocked && linked() {
            return Ok(Route::Link);
        }
        if self.answers_as(peer_id, ip, port).await {
            self.http_unreachable.lock().unwrap().remove(peer_id);
            return Ok(Route::Http(base_url(ip, port)));
        }
        self.http_unreachable.lock().unwrap().insert(peer_id.to_string(), (ip, std::time::Instant::now()));
        if linked() {
            log::warn!("{peer_id}: {ip}:{port} does not accept connections (inbound blocked by its firewall?), sending over the link");
            // and let it know, so its user sees why (newer versions show a "fix" banner)
            self.tell_inbound_blocked(peer_id, ip, port);
            return Ok(Route::Link);
        }
        if self.cfg.alt_addr_fallback {
            if let Some(base) = self.alt_base(peer_id, ip, port).await {
                return Ok(Route::Http(base));
            }
        }
        Err(terr(ErrCode::PeerInboundBlocked, format!("连不上对方 {ip}:{port}，对方也没有连过来（对方的防火墙可能挡住了 Lancat 的入站连接）")))
    }

    /// Opt-in fallback (`NodeConfig::alt_addr_fallback`): the first other address the peer answers
    /// on: addresses it was heard on (newest first), then our online Tailscale peers.
    async fn alt_base(&self, peer_id: &str, first: IpAddr, port: u16) -> Option<String> {
        let mut cands: Vec<(IpAddr, u16)> = self.peer_dial_addrs(peer_id).into_iter().filter(|(ip, _)| *ip != first).collect();
        for ip in netutil::overlay_peers().await.into_iter().take(32) {
            let ip = IpAddr::V4(ip);
            if ip != first && !cands.iter().any(|(c, _)| *c == ip) {
                cands.push((ip, port));
            }
        }
        let ok = futures_util::future::join_all(cands.iter().map(|(ip, p)| self.answers_as(peer_id, *ip, *p))).await;
        let (ip, p) = cands.iter().zip(ok).find(|(_, ok)| *ok).map(|(c, _)| *c)?;
        log::warn!("{peer_id}: {first}:{port} not reachable, sending via {ip}:{p} (alternate address fallback)");
        Some(base_url(ip, p))
    }

    /// `GET /info` at ip:port answers with this device id.
    pub(crate) async fn answers_as(&self, peer_id: &str, ip: IpAddr, port: u16) -> bool {
        let r = self.http.get(format!("{}/info", base_url(ip, port))).header(HEADER_DEVICE_ID, self.me().id).timeout(Duration::from_secs(4)).send().await;
        match r {
            Ok(r) if r.status().is_success() => matches!(json_body::<DeviceInfo>(r).await, Ok(d) if d.id == peer_id),
            _ => false,
        }
    }

    /// Sends the file over the link: `FileStart` -> pieces (binary frames, at most
    /// `LINK_WINDOW_CHUNKS` chunks unacked) -> `FileFinish`. Resumable like the HTTP path (the
    /// receiver keeps what it has). Queued text frames always go out before the next piece.
    async fn link_transfer(self: &Arc<Self>, m: &Message, manifest: &FileManifest, path: &std::path::Path) -> Result<()> {
        struct Unregister<'a>(&'a Node, String);
        impl Drop for Unregister<'_> {
            fn drop(&mut self) {
                self.0.link_waits.lock().unwrap().remove(&self.1);
            }
        }
        let tid = manifest.transfer_id.clone();
        let size = manifest.size;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WsMsg>();
        self.link_waits.lock().unwrap().insert(tid.clone(), tx);
        let _unregister = Unregister(self, tid.clone());
        let lost = || terr(ErrCode::LinkLost, "与对方的连接断开了");
        'attempt: for attempt in 0..2 {
            if !self.send_to(&m.peer_id, &WsMsg::FileStart { manifest: manifest.clone() }) {
                return Err(lost());
            }
            let have = loop {
                let w = recv_reply(&mut rx, 15).await.map_err(|_| terr(ErrCode::PeerOutdated, "对方没有回应经连接传文件的请求（对方的 Lancat 可能是旧版本，需要更新；或者在对方电脑上放行 Lancat 的入站连接）"))?;
                if let WsMsg::FileHave { have, error, gone, code, .. } = w {
                    if gone {
                        return Err(PeerCancelled.into());
                    }
                    if let Some(e) = error {
                        return Err(terr(ErrCode::from_peer(None, code.as_deref()), format!("对方拒绝: {e}")));
                    }
                    break have;
                }
            };
            let have: std::collections::HashSet<u64> = have.into_iter().collect();
            let mut done: u64 = have.iter().map(|&i| chunk_len(size, i)).sum();
            let meter = self.meter(&tid, &m.id, &m.peer_id, size, true);
            meter.emit(self, done, true);
            let (data, relayed) = self.data_path(&m.peer_id).ok_or_else(lost)?;
            let me_id = self.me().id;
            let file = Arc::new(File::open(path)?);
            let mut next = (0..manifest.chunk_count()).filter(|i| !have.contains(i));
            let mut inflight = std::collections::HashSet::new();
            loop {
                while inflight.len() < LINK_WINDOW_CHUNKS {
                    let Some(idx) = next.next() else { break };
                    let len = chunk_len(size, idx) as usize;
                    let f = file.clone();
                    let buf = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                        let mut buf = vec![0u8; len];
                        read_at(&f, &mut buf, idx * CHUNK_SIZE)?;
                        Ok(buf)
                    })
                    .await??;
                    let hash: [u8; 32] = *blake3::hash(&buf).as_bytes();
                    for (k, piece) in buf.chunks(LINK_PIECE).enumerate() {
                        let mut frame = LinkPiece::encode_header(&tid, idx, len as u32, (k * LINK_PIECE) as u32, &hash, piece.len());
                        frame.extend_from_slice(piece);
                        let frame = if relayed { relay_frame(&m.peer_id, &me_id, &frame) } else { frame };
                        data.send(frame).await.map_err(|_| lost())?;
                        meter.add(piece.len() as i64);
                        meter.emit(self, done, false);
                    }
                    inflight.insert(idx);
                }
                if inflight.is_empty() {
                    break;
                }
                let w = recv_reply(&mut rx, 60).await?;
                if let WsMsg::FileChunkAck { chunk, error, gone, code, .. } = w {
                    if !inflight.remove(&chunk) {
                        continue;
                    }
                    if gone {
                        return Err(PeerCancelled.into());
                    }
                    if let Some(e) = error {
                        return Err(terr(ErrCode::from_peer(None, code.as_deref()), format!("chunk {chunk}: {e}")));
                    }
                    done += chunk_len(size, chunk);
                    meter.add(-(chunk_len(size, chunk) as i64));
                    self.touch_peer(&m.peer_id);
                    meter.emit(self, done, true);
                }
            }
            if !self.send_to(&m.peer_id, &WsMsg::FileFinish { transfer_id: tid.clone() }) {
                return Err(lost());
            }
            loop {
                if let WsMsg::FileDone { error, conflict, .. } = recv_reply(&mut rx, 120).await? {
                    match error {
                        None => return Ok(()),
                        Some(_) if conflict && attempt == 0 => {
                            log::warn!("hash mismatch on {}, restarting transfer", manifest.name);
                            continue 'attempt;
                        }
                        Some(e) if conflict => return Err(terr(ErrCode::Verify, format!("完成失败: {e}"))),
                        Some(e) => return Err(terr(ErrCode::PeerRejected, format!("完成失败: {e}"))),
                    }
                }
            }
        }
        Err(terr(ErrCode::Verify, "校验失败"))
    }
}

/// Next reply for a link transfer, or an error after `secs` of silence.
async fn recv_reply(rx: &mut tokio::sync::mpsc::UnboundedReceiver<WsMsg>, secs: u64) -> Result<WsMsg> {
    match tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await {
        Ok(Some(w)) => Ok(w),
        Ok(None) => Err(terr(ErrCode::LinkLost, "与对方的连接断开了")),
        Err(_) => Err(terr(ErrCode::PeerNoResponse, format!("对方 {secs} 秒没有回应"))),
    }
}

fn base_url(ip: IpAddr, port: u16) -> String {
    format!("http://{}", fmt_sa(&SocketAddr::new(ip, port)))
}

fn chunk_len(size: u64, idx: u64) -> u64 {
    CHUNK_SIZE.min(size.saturating_sub(idx * CHUNK_SIZE))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeConfig;

    async fn start(dir: &std::path::Path, name: &str, bind: Option<std::net::IpAddr>) -> Arc<Node> {
        let mut cfg = NodeConfig::new(dir.join("data"));
        cfg.bind_ip = bind;
        cfg.port = 0;
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
        cfg.name_override = Some(name.into());
        cfg.recv_dir_override = Some(dir.join("recv"));
        Node::start(cfg).await.unwrap()
    }

    /// The receiver cannot be reached inbound (it listens on 127.0.0.2 but dials out from
    /// 127.0.0.1, which is all the sender knows of it), but it dialed the link: the file goes over
    /// the link, and a text sent meanwhile is not stuck behind the file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn file_over_link_when_receiver_unreachable() {
        let ts = tempfile::tempdir().unwrap();
        let tr = tempfile::tempdir().unwrap();
        let snd = start(ts.path(), "sender", None).await;
        let rcv = start(tr.path(), "receiver", Some("127.0.0.2".parse().unwrap())).await;
        let rcv_id = rcv.me().id;
        let snd_id = snd.me().id;
        rcv.add_peer_addr(&format!("127.0.0.1:{}", snd.port())).await.unwrap();
        // the sender only knows the receiver by 127.0.0.1 (where nothing listens)
        tokio::time::timeout(Duration::from_secs(5), async {
            while !snd.is_online(&rcv_id) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sender hears the receiver");
        assert_eq!(snd.peer_addr(&rcv_id).unwrap().0.to_string(), "127.0.0.1");
        rcv.dial_link(&snd_id, true).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !snd.links.lock().unwrap().contains_key(&rcv_id) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("link up");

        let size = 3 * CHUNK_SIZE as usize + 12345;
        let data: Vec<u8> = (0..size).map(|i| (i * 7 % 251) as u8).collect();
        let src = ts.path().join("big.bin");
        std::fs::write(&src, &data).unwrap();
        let mut es = snd.subscribe();
        let ms = snd.send_files(&[rcv_id.clone()], &[src]).await.unwrap();
        let fid = ms[0].id.clone();
        // a text while the file is on its way
        tokio::time::sleep(Duration::from_millis(30)).await;
        let t = snd.send_text(&rcv_id, "插队的消息").await.unwrap();
        let mut text_ok = t.status == "delivered";
        let fut = async {
            loop {
                if let Ok(CoreEvent::MessageUpdate { message }) = es.recv().await {
                    if message.id == t.id && message.status == "delivered" {
                        text_ok = true;
                    }
                    if message.id == fid && matches!(message.status.as_str(), "done" | "failed") {
                        return message;
                    }
                }
            }
        };
        let m = tokio::time::timeout(Duration::from_secs(60), fut).await.expect("transfer timed out");
        assert_eq!(m.status, "done", "{:?}", m.body);
        assert!(text_ok || snd.store.message(&t.id).unwrap().unwrap().status == "delivered", "text not delivered");
        let got = std::fs::read(tr.path().join("recv").join("big.bin")).unwrap();
        assert!(got == data, "content differs");
        // the receiver learned that others cannot connect to it (the sender's "I cannot connect to
        // you" after the link came up, and the file arriving over the link)
        let reports = rcv.inbound_reports();
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert_eq!(reports[0].peer_id, snd_id);
        assert!(snd.inbound_reports().is_empty());
        snd.shutdown();
        rcv.shutdown();
    }

    /// The receiver's folder cannot be written: the sender's message fails with
    /// `peer_dir_unwritable`, the receiver's with `local_dir_unwritable` plus a `recv_dir` alert.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn receive_dir_unwritable_is_classified() {
        let ts = tempfile::tempdir().unwrap();
        let tr = tempfile::tempdir().unwrap();
        let snd = start(ts.path(), "sender", None).await;
        // a receive "folder" below a plain file: nothing can be created there
        std::fs::write(tr.path().join("file"), b"x").unwrap();
        let mut cfg = NodeConfig::new(tr.path().join("data"));
        cfg.port = 0;
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
        cfg.name_override = Some("receiver".into());
        cfg.recv_dir_override = Some(tr.path().join("file").join("sub"));
        let rcv = Node::start(cfg).await.unwrap();
        assert!(rcv.check_receive_dir().is_err());
        assert!(snd.check_receive_dir().is_ok());
        let rcv_id = rcv.me().id;
        snd.add_peer_addr(&format!("127.0.0.1:{}", rcv.port())).await.unwrap();
        let mut er = rcv.subscribe();
        let src = ts.path().join("a.txt");
        std::fs::write(&src, b"hello").unwrap();
        let mut es = snd.subscribe();
        let ms = snd.send_files(&[rcv_id.clone()], &[src]).await.unwrap();
        let fid = ms[0].id.clone();
        let m = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Ok(CoreEvent::MessageUpdate { message }) = es.recv().await {
                    if message.id == fid && matches!(message.status.as_str(), "done" | "failed") {
                        return message;
                    }
                }
            }
        })
        .await
        .expect("transfer timed out");
        assert_eq!(m.status, "failed");
        let body: FileBody = serde_json::from_value(m.body).unwrap();
        assert_eq!(body.code.as_deref(), Some("peer_dir_unwritable"), "{:?}", body.error);
        let got_alert = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(CoreEvent::Alert { kind, .. }) = er.recv().await {
                    return kind;
                }
            }
        })
        .await
        .expect("no alert");
        assert_eq!(got_alert, "recv_dir");
        let theirs = rcv.store.message(&fid).unwrap().expect("receiver recorded the file");
        assert_eq!(theirs.status, "failed");
        let tb: FileBody = serde_json::from_value(theirs.body).unwrap();
        assert_eq!(tb.code.as_deref(), Some("local_dir_unwritable"));
        snd.shutdown();
        rcv.shutdown();
    }
}
