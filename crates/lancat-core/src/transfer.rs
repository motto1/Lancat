//! Outgoing file transfers: 4 MiB chunks over HTTP PUT, 4 in flight, resumable, BLAKE3-verified.

use std::fs::File;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::{stream, TryStreamExt};

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
                    error: None,
                };
                let m = Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    peer_id: peer.clone(),
                    outgoing: true,
                    kind: "file".into(),
                    body: serde_json::to_value(&body)?,
                    status: "transferring".into(),
                    ts: now_ms(),
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
        let (status, error) = match res {
            Ok(()) => ("done", None),
            Err(_) if ctl.get() == PAUSE => {
                log::info!("transfer {msg_id} paused");
                ("paused", None)
            }
            Err(_) if ctl.get() == CANCEL => {
                log::info!("transfer {msg_id} cancelled");
                self.send_over_link(&m.peer_id, &WsMsg::FileCancel { transfer_id: body.transfer_id.clone() });
                ("cancelled", None)
            }
            Err(e) if ctl.get() == PEER_CANCEL || e.downcast_ref::<PeerCancelled>().is_some() => {
                log::info!("transfer {msg_id} cancelled by the receiver");
                ("cancelled", Some(PeerCancelled.to_string()))
            }
            Err(e) => {
                log::warn!("transfer {msg_id} failed: {e:#}");
                ("failed", Some(e.to_string()))
            }
        };
        body.error = error;
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
        self.send_over_link(&m.peer_id, &WsMsg::FileCancel { transfer_id: body.transfer_id });
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
            bail!("源文件不存在");
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
        if !self.is_online(&m.peer_id) {
            bail!("对方不在线");
        }
        let base = self.reachable_base(&m.peer_id).await?;
        let me = self.me().id;

        // announce, so the receiver's UI shows it right away
        let offer = WsMsg::FileOffer { id: m.id.clone(), transfer_id: body.transfer_id.clone(), name: body.name.clone(), size, ts: m.ts };
        self.send_over_link(&m.peer_id, &offer);

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
                bail!("对方拒绝: {} {}", code, resp.text().await.unwrap_or_default());
            }
            let status: TransferStatus = json_body(resp).await?;
            let have: std::collections::HashSet<u64> = status.have.into_iter().collect();
            let total_chunks = manifest.chunk_count();
            let missing: Vec<u64> = (0..total_chunks).filter(|i| !have.contains(i)).collect();
            let done_bytes = Arc::new(AtomicU64::new(have.iter().map(|&i| chunk_len(size, i)).sum()));
            self.emit(CoreEvent::Progress { msg_id: m.id.clone(), peer_id: m.peer_id.clone(), done: done_bytes.load(Ordering::Relaxed), total: size, outgoing: true });

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
                            let r = node
                                .http
                                .put(format!("{base}/files/{tid}/{idx}"))
                                .header(HEADER_DEVICE_ID, &me)
                                .header(HEADER_CHUNK_HASH, &hash)
                                .body(data.clone())
                                .timeout(Duration::from_secs(60))
                                .send()
                                .await;
                            match r {
                                Ok(r) if r.status().is_success() => {
                                    last_err = None;
                                    break;
                                }
                                Ok(r) if r.status() == reqwest::StatusCode::GONE => return Err(PeerCancelled.into()),
                                Ok(r) => last_err = Some(anyhow!("chunk {idx}: HTTP {}", r.status())),
                                Err(e) => last_err = Some(anyhow!("chunk {idx}: {e}")),
                            }
                            tokio::time::sleep(Duration::from_millis(300)).await;
                        }
                        if let Some(e) = last_err {
                            return Err(e);
                        }
                        let done = done_bytes.fetch_add(len as u64, Ordering::Relaxed) + len as u64;
                        node.touch_peer(&msg.peer_id);
                        node.emit(CoreEvent::Progress { msg_id: msg.id.clone(), peer_id: msg.peer_id.clone(), done, total: size, outgoing: true });
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
                return Ok(());
            }
            if r.status() == reqwest::StatusCode::CONFLICT && attempt == 0 {
                log::warn!("hash mismatch on {}, restarting transfer", manifest.name);
                continue;
            }
            bail!("完成失败: {} {}", r.status(), r.text().await.unwrap_or_default());
        }
        bail!("校验失败")
    }
}

impl Node {
    /// Base URL of the first address the peer answers on (`GET /info` with its own id), current
    /// address first. Text can still arrive over a link the peer dialed, so a peer whose firewall
    /// drops inbound connections on the address we know looks online and chats fine, while
    /// uploads to that address fail; another address it was heard on (Tailscale ...) may work.
    async fn reachable_base(&self, peer_id: &str) -> Result<String> {
        let mut cands = self.peer_dial_addrs(peer_id);
        let Some(&(first, port)) = cands.first() else { bail!("对方地址未知") };
        if self.answers_as(peer_id, first, port).await {
            return Ok(base_url(first, port));
        }
        // Fallbacks, checked at once: other addresses it was heard on, then our online Tailscale
        // peers (one of them may be this device; it is heard on the LAN only while linked there).
        for ip in netutil::tailscale_peers().await.into_iter().take(32) {
            let ip = IpAddr::V4(ip);
            if !cands.iter().any(|(c, _)| *c == ip) {
                cands.push((ip, port));
            }
        }
        let rest = &cands[1..];
        let ok = futures_util::future::join_all(rest.iter().map(|(ip, p)| self.answers_as(peer_id, *ip, *p))).await;
        if let Some((ip, p)) = rest.iter().zip(ok).find(|(_, ok)| *ok).map(|(c, _)| *c) {
            log::warn!("{peer_id}: {first}:{port} not reachable (inbound blocked by its firewall?), sending via {ip}:{p}");
            return Ok(base_url(ip, p));
        }
        bail!("连不上对方 {first}:{port}（对方能连我们所以文字可达，但对方的防火墙挡住了 Lancat 的入站连接，文件发不过去）")
    }

    /// `GET /info` at ip:port answers with this device id.
    async fn answers_as(&self, peer_id: &str, ip: IpAddr, port: u16) -> bool {
        let r = self.http.get(format!("{}/info", base_url(ip, port))).header(HEADER_DEVICE_ID, self.me().id).timeout(Duration::from_secs(4)).send().await;
        match r {
            Ok(r) if r.status().is_success() => matches!(json_body::<DeviceInfo>(r).await, Ok(d) if d.id == peer_id),
            _ => false,
        }
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

    async fn start(dir: &std::path::Path, name: &str) -> Arc<Node> {
        let mut cfg = NodeConfig::new(dir.join("data"));
        cfg.port = 0;
        cfg.enable_mdns = false;
        cfg.enable_udp = false;
        cfg.name_override = Some(name.into());
        cfg.recv_dir_override = Some(dir.join("recv"));
        Node::start(cfg).await.unwrap()
    }

    /// The address we know the peer by drops connections (its firewall), but it was also heard on
    /// another one: the file goes there instead of failing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn file_falls_back_to_another_address() {
        let ta = tempfile::tempdir().unwrap();
        let tb = tempfile::tempdir().unwrap();
        let a = start(ta.path(), "alpha").await;
        let b = start(tb.path(), "bravo").await;
        let pb = a.add_peer_addr(&format!("127.0.0.1:{}", b.port())).await.unwrap();
        // the "LAN address" that is blackholed (TEST-NET-1: never answers)
        let dead: IpAddr = "192.0.2.1".parse().unwrap();
        a.set_peer_addr(&pb.id, dead);
        assert_eq!(a.peer_addr(&pb.id).unwrap().0, dead);
        assert_eq!(a.peer_dial_addrs(&pb.id).iter().map(|x| x.0).collect::<Vec<_>>(), vec![dead, "127.0.0.1".parse::<IpAddr>().unwrap()]);

        let src = ta.path().join("fb.bin");
        std::fs::write(&src, vec![7u8; 5 * 1024 * 1024 + 3]).unwrap();
        let mut ea = a.subscribe();
        let ms = a.send_files(&[pb.id.clone()], &[src]).await.unwrap();
        let id = ms[0].id.clone();
        let fut = async {
            loop {
                if let Ok(CoreEvent::MessageUpdate { message }) = ea.recv().await {
                    if message.id == id && matches!(message.status.as_str(), "done" | "failed") {
                        return message;
                    }
                }
            }
        };
        let m = tokio::time::timeout(Duration::from_secs(30), fut).await.expect("transfer timed out");
        assert_eq!(m.status, "done", "{:?}", m.body);
        let got = std::fs::read(tb.path().join("recv").join("fb.bin")).unwrap();
        assert_eq!(got.len(), 5 * 1024 * 1024 + 3);
        a.shutdown();
        b.shutdown();
    }
}
