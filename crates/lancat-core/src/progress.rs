//! Live byte progress for file transfers. Progress used to move only when a whole 4 MiB chunk
//! was confirmed, and with 4 chunks in flight a file under 16 MiB went 0 % → 100 % in one step
//! (larger ones in 16 MiB jumps). A `Meter` counts the bytes of chunks still on the way (sent
//! into the request body / link, or read from it on the receiving side) on top of the confirmed
//! ones, and emits at most every 100 ms. Keyed by transfer id; both sides have one.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::events::CoreEvent;
use crate::node::Node;

const EVERY_MS: u64 = 100;
/// Body slices for an outgoing chunk: progress steps of 64 KiB.
pub(crate) const SLICE: usize = 64 * 1024;

pub(crate) struct Meter {
    msg_id: String,
    peer_id: String,
    total: u64,
    outgoing: bool,
    /// Bytes of chunks not confirmed yet (may briefly go negative while counters settle).
    live: AtomicI64,
    last_ms: AtomicU64,
    start: Instant,
}

impl Meter {
    pub(crate) fn add(&self, n: i64) {
        self.live.fetch_add(n, Ordering::Relaxed);
    }

    pub(crate) fn reset(&self) {
        self.live.store(0, Ordering::Relaxed);
    }

    /// Emits confirmed bytes (`confirmed()`) plus the bytes in flight: at most every 100 ms
    /// unless `force`.
    pub(crate) fn emit_with(&self, node: &Node, force: bool, confirmed: impl FnOnce() -> u64) {
        let now = self.start.elapsed().as_millis() as u64 + EVERY_MS;
        if !force && now.saturating_sub(self.last_ms.load(Ordering::Relaxed)) < EVERY_MS {
            return;
        }
        self.last_ms.store(now, Ordering::Relaxed);
        let done = (confirmed() as i64 + self.live.load(Ordering::Relaxed).max(0)).clamp(0, self.total as i64) as u64;
        node.emit(CoreEvent::Progress { msg_id: self.msg_id.clone(), peer_id: self.peer_id.clone(), done, total: self.total, outgoing: self.outgoing });
    }

    pub(crate) fn emit(&self, node: &Node, confirmed: u64, force: bool) {
        self.emit_with(node, force, || confirmed);
    }
}

impl Node {
    /// The meter of a transfer, created on first use; its in-flight count starts over.
    pub(crate) fn meter(&self, tid: &str, msg_id: &str, peer_id: &str, total: u64, outgoing: bool) -> Arc<Meter> {
        let mut m = self.meters.lock().unwrap();
        let mt = m
            .entry(tid.to_string())
            .or_insert_with(|| {
                Arc::new(Meter {
                    msg_id: msg_id.to_string(),
                    peer_id: peer_id.to_string(),
                    total,
                    outgoing,
                    live: AtomicI64::new(0),
                    last_ms: AtomicU64::new(0),
                    start: Instant::now(),
                })
            })
            .clone();
        mt.reset();
        mt
    }

    pub(crate) fn meter_get(&self, tid: &str) -> Option<Arc<Meter>> {
        self.meters.lock().unwrap().get(tid).cloned()
    }

    pub(crate) fn meter_drop(&self, tid: &str) {
        self.meters.lock().unwrap().remove(tid);
    }
}

/// An outgoing chunk as a streamed request body that counts each 64 KiB slice as it is handed
/// to the connection (`sent` collects them, so a failed attempt can take them back).
pub(crate) fn counting_body(data: axum::body::Bytes, meter: Arc<Meter>, node: Arc<Node>, confirmed: Arc<AtomicU64>, sent: Arc<AtomicU64>) -> reqwest::Body {
    let len = data.len();
    let s = futures_util::stream::iter((0..len).step_by(SLICE).map(move |o| {
        let b = data.slice(o..(o + SLICE).min(len));
        meter.add(b.len() as i64);
        sent.fetch_add(b.len() as u64, Ordering::Relaxed);
        meter.emit_with(&node, false, || confirmed.load(Ordering::Relaxed));
        Ok::<_, std::io::Error>(b)
    }));
    reqwest::Body::wrap_stream(s)
}
