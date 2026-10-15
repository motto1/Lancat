//! axum HTTP/WebSocket server on TCP 47820.
//!
//! Routes:
//! - `GET  /info`                   device info
//! - `GET  /ws`                     WebSocket link (text/emoji/typing/ack/file_offer)
//! - `POST /msg`                    HTTP fallback for one WsMsg (reply body = optional WsMsg)
//! - `POST /files/{tid}`            start/resume an incoming transfer (body: FileManifest) -> TransferStatus
//! - `GET  /files/{tid}`            TransferStatus (resume)
//! - `PUT  /files/{tid}/{chunk}`    one 4 MiB chunk (header x-lancat-chunk-hash = BLAKE3 hex)
//! - `POST /finish/{tid}`           verify BLAKE3 + move into the receive dir

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};

use crate::events::CoreEvent;
use crate::files::IncomingTransfer;
use crate::node::Node;
use crate::protocol::*;
use crate::store::FileBody;

type St = State<Arc<Node>>;

/// Chunk uploads buffered and written at once, over all senders (each body is up to 4 MiB and is
/// read into memory): bounds the receiver's memory however many devices send at the same time.
pub(crate) const MAX_INCOMING_CHUNKS: usize = 16;

pub(crate) fn router(node: Arc<Node>) -> Router {
    Router::new()
        .route("/info", get(info))
        .route("/ws", get(ws))
        .route("/msg", post(msg))
        .route("/files/{tid}", post(file_init).get(file_status))
        .route("/files/{tid}/{chunk}", put(file_chunk))
        .route("/finish/{tid}", post(file_finish))
        .layer(DefaultBodyLimit::max((CHUNK_SIZE as usize) * 2))
        .layer(middleware::from_fn_with_state(node.clone(), guard))
        .with_state(node)
}

pub(crate) fn spawn(node: Arc<Node>, listener: tokio::net::TcpListener) {
    let mut stop = node.shutdown_rx();
    let app = router(node);
    // TCP_NODELAY on accepted connections too (WebSocket frames, small HTTP replies)
    use axum::serve::ListenerExt;
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    tokio::spawn(async move {
        let svc = app.into_make_service_with_connect_info::<SocketAddr>();
        let res = axum::serve(listener, svc)
            .with_graceful_shutdown(async move {
                let _ = stop.changed().await;
            })
            .await;
        if let Err(e) = res {
            log::error!("server stopped: {e}");
        }
    });
}

/// Rejects non-LAN sources and blacklisted devices / addresses.
async fn guard(State(node): St, ConnectInfo(addr): ConnectInfo<SocketAddr>, req: Request, next: Next) -> Response {
    let ip = addr.ip();
    // through a tunnel / from outside: only the link, and only paired (remote.rs; checked in `ws`)
    if node.is_public_request(&ip, req.headers()) {
        if req.uri().path() != "/ws" || !node.store.settings().public_access {
            return (StatusCode::FORBIDDEN, "lan only").into_response();
        }
        return next.run(req).await;
    }
    if !node.is_trusted_source(&ip) {
        return (StatusCode::FORBIDDEN, "lan only").into_response();
    }
    if node.is_blocked_addr(&ip) {
        return (StatusCode::FORBIDDEN, "blocked").into_response();
    }
    if let Some(id) = req.headers().get(HEADER_DEVICE_ID).and_then(|v| v.to_str().ok()) {
        if node.store.is_blocked(id) {
            return (StatusCode::FORBIDDEN, "blocked").into_response();
        }
        // a peer got in: inbound connections work (runtime signal for diagnostics)
        if !id.is_empty() && id != node.me().id {
            node.note_inbound(id);
        }
    }
    next.run(req).await
}

fn sender(h: &HeaderMap) -> Result<String, (StatusCode, String)> {
    h.get(HEADER_DEVICE_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .map(String::from)
        .ok_or((StatusCode::BAD_REQUEST, "missing device id".into()))
}

async fn info(State(node): St) -> Json<DeviceInfo> {
    Json(node.me())
}

async fn ws(State(node): St, ConnectInfo(addr): ConnectInfo<SocketAddr>, h: HeaderMap, up: WebSocketUpgrade) -> Response {
    let up = up.max_message_size(4 << 20);
    if !node.is_public_request(&addr.ip(), &h) {
        return up.on_upgrade(move |socket| node.accept_ws(socket, addr.ip(), crate::remote::LinkCtx::Lan));
    }
    let ctx = match node.check_public_auth(&h) {
        Ok(c) => c,
        Err(why) => {
            log::info!("public link from {} refused: {why}", addr.ip());
            return (StatusCode::UNAUTHORIZED, why).into_response();
        }
    };
    let tok = match &ctx {
        crate::remote::LinkCtx::RemoteAccept { new_token: Some(t), .. } => Some(t.clone()),
        _ => None,
    };
    let mut resp = up.on_upgrade(move |socket| node.accept_ws(socket, crate::remote::REMOTE_IP, ctx));
    if let Some(t) = tok.and_then(|t| t.parse().ok()) {
        resp.headers_mut().insert(HEADER_TOKEN, t);
    }
    resp
}

async fn msg(State(node): St, ConnectInfo(addr): ConnectInfo<SocketAddr>, h: HeaderMap, body: Bytes) -> Result<Response, (StatusCode, String)> {
    let from = sender(&h)?;
    let wire: WsMsg = serde_json::from_slice(&body).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if let WsMsg::Hello { device } = &wire {
        if device.id == from {
            node.peer_seen(device.clone(), addr.ip());
        }
    }
    match node.handle_wire(&from, wire, Some(addr.ip())).await {
        Some(reply) => Ok(Json(reply).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

/// This side cancelled the transfer: the sender must stop instead of retrying.
fn cancelled_here(node: &Node, from: &str, tid: &str) -> bool {
    matches!(node.store.message_by_transfer(from, tid), Ok(Some(m)) if m.status == "cancelled")
}

fn err500(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn file_init(State(node): St, Path(tid): Path<String>, h: HeaderMap, Json(m): Json<FileManifest>) -> Result<Json<TransferStatus>, (StatusCode, String)> {
    let from = sender(&h)?;
    if m.transfer_id != tid {
        return Err((StatusCode::BAD_REQUEST, "manifest mismatch".into()));
    }
    node.incoming_init(&from, m).await.map(Json)
}

async fn file_status(State(node): St, Path(tid): Path<String>) -> Result<Json<TransferStatus>, StatusCode> {
    let t = node.incoming.lock().unwrap().get(&tid).cloned().ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(TransferStatus { have: t.have(), complete: t.is_complete() }))
}

async fn file_chunk(State(node): St, Path((tid, chunk)): Path<(String, u64)>, h: HeaderMap, body: axum::body::Body) -> Result<StatusCode, (StatusCode, String)> {
    let from = sender(&h)?;
    // take a slot before reading the body, so waiting uploads are not held in memory
    let _slot = node.chunk_slots.acquire().await.map_err(err500)?;
    // read it in frames as they arrive, so the progress moves within the chunk (progress.rs)
    let meter = node.meter_get(&tid);
    let confirmed = || node.incoming.lock().unwrap().get(&tid).map(|t| t.received_bytes()).unwrap_or(0);
    let max = (CHUNK_SIZE as usize) * 2;
    let mut buf: Vec<u8> = Vec::with_capacity(CHUNK_SIZE as usize);
    let mut frames = body.into_data_stream();
    while let Some(f) = futures_util::StreamExt::next(&mut frames).await {
        let f = match f {
            Ok(f) if buf.len() + f.len() <= max => f,
            r => {
                if let Some(mt) = &meter {
                    mt.add(-(buf.len() as i64));
                }
                return Err((StatusCode::BAD_REQUEST, r.err().map(|e| e.to_string()).unwrap_or_else(|| "chunk too large".into())));
            }
        };
        buf.extend_from_slice(&f);
        if let Some(mt) = &meter {
            mt.add(f.len() as i64);
            mt.emit_with(&node, false, confirmed);
        }
    }
    let hash = h.get(HEADER_CHUNK_HASH).and_then(|v| v.to_str().ok()).map(String::from);
    let streamed = if meter.is_some() { buf.len() as u64 } else { 0 };
    node.incoming_chunk(&from, &tid, chunk, buf, hash, streamed).await?;
    Ok(StatusCode::OK)
}

async fn file_finish(State(node): St, Path(tid): Path<String>, h: HeaderMap) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let from = sender(&h)?;
    node.incoming_finish(&from, &tid).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// The receiving side of a transfer, shared by the HTTP routes and the link (`WsMsg::FileStart`
/// / binary pieces / `FileFinish`). Errors carry the HTTP status the route answers with.
impl Node {
    pub(crate) async fn incoming_init(self: &Arc<Self>, from: &str, m: FileManifest) -> Result<TransferStatus, (StatusCode, String)> {
        let tid = m.transfer_id.clone();
        if m.from != from {
            return Err((StatusCode::BAD_REQUEST, "manifest mismatch".into()));
        }
        if cancelled_here(self, from, &tid) {
            return Err((StatusCode::GONE, "cancelled".into()));
        }
        let max_mb = self.store.settings().max_file_mb;
        if max_mb > 0 && m.size > max_mb * 1024 * 1024 {
            return Err((StatusCode::PAYLOAD_TOO_LARGE, format!("file exceeds {max_mb} MB")));
        }
        let existing = self.incoming.lock().unwrap().get(&tid).cloned();
        let t = match existing {
            Some(t) if t.manifest.hash == m.hash => t,
            _ => {
                let dir = self.receive_dir();
                let mc = m.clone();
                let t = match tokio::task::spawn_blocking(move || IncomingTransfer::open(&dir, mc)).await.map_err(err500)? {
                    Ok(t) => t,
                    // receive folder not writable / disk full: 507, and our side shows why
                    Err(e) => return Err(self.incoming_storage_failed(from, &m, &e, true)),
                };
                let t = Arc::new(t);
                self.incoming.lock().unwrap().insert(tid.clone(), t.clone());
                t
            }
        };
        self.meter(&tid, &m.msg_id, from, m.size, false);
        // make sure the conversation shows the file even if the offer never arrived
        let body = FileBody { name: m.name.clone(), size: m.size, transfer_id: tid.clone(), hash: m.hash.clone(), ..Default::default() };
        if !self.store_incoming(from, &m.msg_id, "file", serde_json::to_value(&body).unwrap_or_default(), "receiving", m.ts) {
            return Err(err500("could not record the transfer"));
        }
        if let Ok(Some(msg)) = self.store.message(&m.msg_id) {
            if msg.status != "receiving" {
                self.set_status(&m.msg_id, "receiving");
            }
        }
        // zero-length files: nothing to upload
        Ok(TransferStatus { have: t.have(), complete: t.is_complete() })
    }

    /// `streamed`: bytes of this chunk already counted as in flight on the transfer's meter.
    pub(crate) async fn incoming_chunk(&self, from: &str, tid: &str, chunk: u64, body: Vec<u8>, hash: Option<String>, streamed: u64) -> Result<(), (StatusCode, String)> {
        // written or not, these bytes stop being "in flight" (on success they are confirmed)
        struct Settle(Option<Arc<crate::progress::Meter>>, u64);
        impl Drop for Settle {
            fn drop(&mut self) {
                if let Some(m) = &self.0 {
                    m.add(-(self.1 as i64));
                }
            }
        }
        let settle = Settle(self.meter_get(tid), streamed);
        let Some(t) = self.incoming.lock().unwrap().get(tid).cloned() else {
            if cancelled_here(self, from, tid) {
                return Err((StatusCode::GONE, "cancelled".into()));
            }
            return Err((StatusCode::NOT_FOUND, "unknown transfer".into()));
        };
        if t.manifest.from != from {
            return Err((StatusCode::FORBIDDEN, "not your transfer".into()));
        }
        let t2 = t.clone();
        let written = tokio::task::spawn_blocking(move || t2.write_chunk(chunk, &body, hash.as_deref())).await.map_err(err500)?;
        if let Err(e) = written {
            if crate::errors::chain_is_storage_error(&e) {
                return Err(self.incoming_storage_failed(from, &t.manifest, &e, false));
            }
            return Err((StatusCode::UNPROCESSABLE_ENTITY, e.to_string()));
        }
        self.touch_peer(from);
        let meter = settle.0.clone();
        drop(settle);
        match meter {
            Some(mt) => mt.emit(self, t.received_bytes(), true),
            None => self.emit(CoreEvent::Progress {
                msg_id: t.manifest.msg_id.clone(),
                peer_id: from.to_string(),
                done: t.received_bytes(),
                total: t.manifest.size,
                outgoing: false,
            }),
        }
        Ok(())
    }

    pub(crate) async fn incoming_finish(&self, from: &str, tid: &str) -> Result<(), (StatusCode, String)> {
        self.meter_drop(tid);
        let t = self.incoming.lock().unwrap().get(tid).cloned().ok_or((StatusCode::NOT_FOUND, "unknown transfer".to_string()))?;
        if t.manifest.from != from {
            return Err((StatusCode::FORBIDDEN, "not your transfer".into()));
        }
        let dir = self.receive_dir();
        let t2 = t.clone();
        let res = tokio::task::spawn_blocking(move || t2.finish(&dir)).await.map_err(err500)?;
        match res {
            Ok(path) => {
                self.incoming.lock().unwrap().remove(tid);
                let body = FileBody {
                    name: t.manifest.name.clone(),
                    size: t.manifest.size,
                    transfer_id: tid.to_string(),
                    path: path.to_string_lossy().to_string(),
                    hash: t.manifest.hash.clone(),
                    ..Default::default()
                };
                if let Ok(Some(m)) = self.store.update_message(&t.manifest.msg_id, "done", Some(&serde_json::to_value(&body).unwrap_or_default())) {
                    self.emit(CoreEvent::MessageUpdate { message: m });
                }
                log::info!("received {} -> {}", t.manifest.name, path.display());
                Ok(())
            }
            Err(e) if crate::errors::chain_is_storage_error(&e) => Err(self.incoming_storage_failed(from, &t.manifest, &e, false)),
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("hash mismatch") {
                    Err((StatusCode::CONFLICT, msg))
                } else {
                    Err((StatusCode::UNPROCESSABLE_ENTITY, msg))
                }
            }
        }
    }
}
