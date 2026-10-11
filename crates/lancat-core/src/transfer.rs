//! Outgoing file transfers: 4 MiB chunks over HTTP PUT, 4 in flight, resumable, BLAKE3-verified.

use std::fs::File;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::{stream, StreamExt};

use crate::events::CoreEvent;
use crate::files::{discard_partial, expand_paths, hash_file, read_at};
use crate::node::{fmt_sa, json_body, Node};
use crate::protocol::*;
use crate::store::{FileBody, Message};

pub const PARALLEL_CHUNKS: usize = 4;

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
            r = self.transfer_inner(&msg_id) => r,
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
        self.incoming.lock().unwrap().remove(transfer_id);
        let dir = self.receive_dir();
        let tid = transfer_id.to_string();
        self.rt.spawn(async move {
            for _ in 0..20 {
                if discard_partial(&dir, &tid) {
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
        let (ip, port) = self.peer_addr(&m.peer_id).ok_or_else(|| anyhow!("对方地址未知"))?;
        let base = format!("http://{}", fmt_sa(&SocketAddr::new(ip, port)));
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
            let results: Vec<Result<()>> = stream::iter(missing)
                .map(|idx| {
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
                .buffer_unordered(PARALLEL_CHUNKS)
                .collect()
                .await;
            if let Some(Err(e)) = results.into_iter().find(|r| r.is_err()) {
                return Err(e);
            }

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

fn chunk_len(size: u64, idx: u64) -> u64 {
    CHUNK_SIZE.min(size.saturating_sub(idx * CHUNK_SIZE))
}

