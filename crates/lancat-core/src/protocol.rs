//! Wire protocol shared by all Lancat peers.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1";
pub const SERVICE_TYPE: &str = "_lancat._tcp.local.";
pub const DEFAULT_PORT: u16 = 47820;
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;
/// A discovery burst: probes go out at these offsets (seconds) after a trigger, then stop.
pub const PROBE_BURST_SECS: [u64; 3] = [0, 1, 3];
/// Fallback while no peer is online: one probe round this often.
pub const IDLE_PROBE_SECS: u64 = 180;
/// How often the tailnet peer list is re-read to find devices that came online later.
pub const TS_RELIST_SECS: u64 = 60;
/// How often the network watcher compares interface addresses / checks for a wake-up.
pub const NETWATCH_SECS: u64 = 5;
/// Link keepalive: ping every 30 s; the link is dropped after 2 silent intervals (60–90 s).
pub const LINK_PING_SECS: u64 = 30;
pub const LINK_MISSED_PINGS: u32 = 2;
/// A peer that is online but has no link (and no other traffic) goes offline after this.
pub const LINKLESS_GRACE_SECS: u64 = 20;
/// After an unexpected link close the peer is re-probed and marked offline if it does not answer in this time.
pub const LINK_LOST_GRACE_SECS: u64 = 3;
pub const HEADER_DEVICE_ID: &str = "x-lancat-id";
pub const HEADER_CHUNK_HASH: &str = "x-lancat-chunk-hash";

/// What a device says about itself (mDNS TXT, UDP probe/reply, WS hello, GET /info).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub ver: String,
    pub port: u16,
}

/// What a UDP discovery datagram means.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum BeaconOp {
    /// "Who's there?" — receivers add the sender and reply by unicast.
    Probe,
    /// Unicast answer to a probe.
    Reply,
    /// Clean exit: mark the sender offline now.
    Bye,
    /// Old (≤ c3ecf32) periodic heartbeat without an `op` field: just "I'm here".
    #[default]
    Announce,
}

/// UDP discovery payload (port 47820). Older peers ignore `op` and treat it as a heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beacon {
    pub lancat: u8,
    #[serde(default)]
    pub op: BeaconOp,
    pub device: DeviceInfo,
}

/// Emoji payload: either a built-in Noto animated emoji (by codepoint) or a custom
/// sticker (Lottie JSON inline, e.g. imported from .tgs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmojiPayload {
    /// "noto" | "sticker"
    pub kind: String,
    /// codepoint (e.g. "1f600") or sticker id
    pub code: String,
    /// The emoji character / display name, used as fallback text.
    #[serde(default)]
    pub alt: String,
    /// Lottie JSON for custom stickers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// Messages over the per-peer WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsMsg {
    Hello { device: DeviceInfo },
    Text { id: String, text: String, ts: i64 },
    Emoji { id: String, emoji: EmojiPayload, ts: i64 },
    /// Announces an incoming file before chunks arrive (UI shows it immediately).
    FileOffer { id: String, transfer_id: String, name: String, size: u64, ts: i64 },
    Typing { typing: bool },
    Ack { id: String },
    Ping,
    Pong,
    /// Clean exit: the sender is going away, mark it offline immediately.
    Bye,
    /// Either side gave up a file transfer: the sender stops, the receiver drops the partial file.
    /// Older peers ignore it (unknown messages are skipped on the link).
    FileCancel { transfer_id: String },
    // ---- file transfer over the link itself, for when the receiver cannot be reached by HTTP
    // (its firewall drops inbound connections, but it dialed this link). Same steps as the HTTP
    // routes: start (= POST /files) -> chunks as binary frames (see `LinkPiece`) acked one by one
    // -> finish. Older peers ignore these (no `FileHave` comes back: the sender gives up).
    /// sender -> receiver: start / resume a transfer.
    FileStart { manifest: FileManifest },
    /// receiver -> sender: answer to `FileStart`.
    FileHave {
        transfer_id: String,
        #[serde(default)]
        have: Vec<u64>,
        #[serde(default)]
        complete: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// The receiver cancelled this transfer (HTTP 410).
        #[serde(default)]
        gone: bool,
        /// Why it refused, machine-readable (`ErrCode::wire_for_status`: "dir_unwritable",
        /// "too_large"). Optional: older receivers never send it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    /// receiver -> sender: one chunk (all its pieces) was written, or failed.
    FileChunkAck {
        transfer_id: String,
        chunk: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default)]
        gone: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    /// sender -> receiver: every chunk was acked, verify and move into place (= POST /finish).
    FileFinish { transfer_id: String },
    /// receiver -> sender: answer to `FileFinish`.
    FileDone {
        transfer_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// Whole-file hash mismatch (HTTP 409): the sender starts over once.
        #[serde(default)]
        conflict: bool,
    },
    /// "I cannot connect to you": the peer tried `addr:port` (where we listen, as it knows us)
    /// and got no answer, so it only reaches us over a link we dialed. Tells this device that its
    /// own inbound connections are blocked (firewall, "Public" network) so it can warn its user.
    /// Optional and informational: older peers fail to parse it and skip it.
    InboundBlocked { addr: String, port: u16 },
    /// Gossip: the peers the sender has a link to right now (relay.rs). Sent on link changes and
    /// every 45 s. Older peers skip it.
    Peers { peers: Vec<PeerHint> },
    /// A message for `to` from `from`, through the peer this link goes to (one hop, relay.rs).
    /// `msg` is a `WsMsg` as JSON (kept raw so a relay forwards types it does not know).
    Relay { to: String, from: String, msg: serde_json::Value },
}

/// One entry of `WsMsg::Peers`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerHint {
    pub device: DeviceInfo,
    /// Addresses the gossiping peer knows it by, best first.
    #[serde(default)]
    pub addrs: Vec<String>,
    /// Which connection the gossiping peer has to it (changes when that link is re-made): a
    /// relayed path that was re-made may have lost what went through it, so queued messages are
    /// sent again. 0 from older versions.
    #[serde(default)]
    pub link: u64,
}

const RELAY_MAGIC: &[u8; 4] = b"LCR1";

/// A binary link frame for `to`, sent through a relay:
/// magic(4) | to_len(1) | to | from_len(1) | from | inner frame
pub fn relay_frame(to: &str, from: &str, inner: &[u8]) -> Vec<u8> {
    let (t, f) = (to.as_bytes(), from.as_bytes());
    let (t, f) = (&t[..t.len().min(255)], &f[..f.len().min(255)]);
    let mut v = Vec::with_capacity(6 + t.len() + f.len() + inner.len());
    v.extend_from_slice(RELAY_MAGIC);
    v.push(t.len() as u8);
    v.extend_from_slice(t);
    v.push(f.len() as u8);
    v.extend_from_slice(f);
    v.extend_from_slice(inner);
    v
}

pub fn is_relay_frame(b: &[u8]) -> bool {
    b.len() >= 4 && &b[..4] == RELAY_MAGIC
}

/// (to, from, inner frame)
pub fn relay_unframe(b: &[u8]) -> Option<(String, String, &[u8])> {
    if !is_relay_frame(b) {
        return None;
    }
    let tl = *b.get(4)? as usize;
    let to = std::str::from_utf8(b.get(5..5 + tl)?).ok()?.to_string();
    let fl = *b.get(5 + tl)? as usize;
    let s = 6 + tl;
    let from = std::str::from_utf8(b.get(s..s + fl)?).ok()?.to_string();
    Some((to, from, b.get(s + fl..)?))
}

impl WsMsg {
    /// The transfer a link-transfer reply belongs to (routed to the waiting sender).
    pub fn link_reply_tid(&self) -> Option<&str> {
        match self {
            WsMsg::FileHave { transfer_id, .. } | WsMsg::FileChunkAck { transfer_id, .. } | WsMsg::FileDone { transfer_id, .. } => Some(transfer_id),
            _ => None,
        }
    }
}

/// Size of one binary frame's payload when a chunk travels over the link: small enough that a
/// text / ack / ping queued behind it waits only for one piece, not for a whole 4 MiB chunk.
pub const LINK_PIECE: usize = 256 * 1024;
/// Chunks of one transfer sent over the link before the first of them is acked (flow control).
pub const LINK_WINDOW_CHUNKS: usize = 2;
const PIECE_MAGIC: &[u8; 4] = b"LCF1";

/// One binary link frame: a piece of a chunk. Every piece carries the chunk's length and BLAKE3
/// hash, so pieces need no separate announcement and the receiver can verify the whole chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkPiece {
    pub transfer_id: String,
    pub chunk: u64,
    pub chunk_len: u32,
    pub offset: u32,
    pub hash: [u8; 32],
    pub data: Vec<u8>,
}

impl LinkPiece {
    /// magic(4) | tid_len(1) | tid | chunk(u64 BE) | chunk_len(u32 BE) | offset(u32 BE) | hash(32) | data
    pub fn encode_header(transfer_id: &str, chunk: u64, chunk_len: u32, offset: u32, hash: &[u8; 32], data_len: usize) -> Vec<u8> {
        let t = transfer_id.as_bytes();
        let mut v = Vec::with_capacity(4 + 1 + t.len() + 8 + 4 + 4 + 32 + data_len);
        v.extend_from_slice(PIECE_MAGIC);
        v.push(t.len().min(255) as u8);
        v.extend_from_slice(&t[..t.len().min(255)]);
        v.extend_from_slice(&chunk.to_be_bytes());
        v.extend_from_slice(&chunk_len.to_be_bytes());
        v.extend_from_slice(&offset.to_be_bytes());
        v.extend_from_slice(hash);
        v
    }

    pub fn decode(b: &[u8]) -> Option<LinkPiece> {
        if b.len() < 5 || &b[..4] != PIECE_MAGIC {
            return None;
        }
        let tl = b[4] as usize;
        let rest = b.get(5..)?;
        let tid = std::str::from_utf8(rest.get(..tl)?).ok()?.to_string();
        let r = rest.get(tl..)?;
        if r.len() < 8 + 4 + 4 + 32 {
            return None;
        }
        let chunk = u64::from_be_bytes(r[0..8].try_into().ok()?);
        let chunk_len = u32::from_be_bytes(r[8..12].try_into().ok()?);
        let offset = u32::from_be_bytes(r[12..16].try_into().ok()?);
        let hash: [u8; 32] = r[16..48].try_into().ok()?;
        Some(LinkPiece { transfer_id: tid, chunk, chunk_len, offset, hash, data: r[48..].to_vec() })
    }
}

/// POST /files/{transfer_id}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileManifest {
    pub transfer_id: String,
    pub msg_id: String,
    pub from: String,
    /// Relative path (folders keep their structure), '/' separated.
    pub name: String,
    pub size: u64,
    pub chunk_size: u64,
    /// BLAKE3 hex of the whole file.
    pub hash: String,
    pub ts: i64,
}

impl FileManifest {
    pub fn chunk_count(&self) -> u64 {
        chunk_count(self.size, self.chunk_size)
    }
}

pub fn chunk_count(size: u64, chunk: u64) -> u64 {
    if size == 0 {
        0
    } else {
        size.div_ceil(chunk)
    }
}

/// Response of POST/GET /files/{transfer_id}: which chunks are already on disk.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TransferStatus {
    pub have: Vec<u64>,
    pub complete: bool,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

static LAST_STAMP: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Timestamp of a message entering this device's history, sent or received: our clock, strictly
/// increasing. Conversations are ordered by it, so they show things in the order they happened
/// here. Received messages used to keep the sender's clock, and a peer running a few minutes
/// fast put every reply we sent afterwards *above* its message, out of sight.
pub fn msg_stamp() -> i64 {
    use std::sync::atomic::Ordering::SeqCst;
    let now = now_ms();
    let mut last = LAST_STAMP.load(SeqCst);
    loop {
        let next = now.max(last + 1);
        match LAST_STAMP.compare_exchange_weak(last, next, SeqCst, SeqCst) {
            Ok(_) => return next,
            Err(v) => last = v,
        }
    }
}

/// Never stamp below the newest stored message (our clock may have been set back), unless that
/// one lies implausibly far in the future.
pub fn seed_msg_stamp(newest: i64) {
    if newest <= now_ms() + 600_000 {
        LAST_STAMP.fetch_max(newest, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_roundtrip() {
        let m = WsMsg::Text { id: "a".into(), text: "你好 👋".into(), ts: 1 };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"type\":\"text\""));
        assert_eq!(serde_json::from_str::<WsMsg>(&s).unwrap(), m);
        let e = WsMsg::Emoji {
            id: "b".into(),
            emoji: EmojiPayload { kind: "noto".into(), code: "1f600".into(), alt: "😀".into(), data: None },
            ts: 2,
        };
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains("data"));
        assert_eq!(serde_json::from_str::<WsMsg>(&s).unwrap(), e);
        assert_eq!(serde_json::from_str::<WsMsg>(r#"{"type":"ping"}"#).unwrap(), WsMsg::Ping);
        assert_eq!(serde_json::from_str::<WsMsg>(r#"{"type":"bye"}"#).unwrap(), WsMsg::Bye);
    }

    #[test]
    fn beacon_compat() {
        let d = DeviceInfo { id: "x".into(), name: "n".into(), os: "linux".into(), ver: "0.1.0".into(), port: 47820 };
        // an old heartbeat (no op) still parses, as a plain announce
        let old = r#"{"lancat":1,"device":{"id":"x","name":"n","os":"linux","ver":"0.1.0","port":47820}}"#;
        let b: Beacon = serde_json::from_str(old).unwrap();
        assert_eq!(b.op, BeaconOp::Announce);
        assert_eq!(b.device, d);
        let s = serde_json::to_string(&Beacon { lancat: 1, op: BeaconOp::Probe, device: d }).unwrap();
        assert!(s.contains(r#""op":"probe""#));
        assert_eq!(serde_json::from_str::<Beacon>(&s).unwrap().op, BeaconOp::Probe);
    }

    #[test]
    fn link_piece_roundtrip() {
        let h = [7u8; 32];
        let mut f = LinkPiece::encode_header("tid-1", 3, 1000, 256, &h, 4);
        f.extend_from_slice(b"abcd");
        let p = LinkPiece::decode(&f).unwrap();
        assert_eq!(p, LinkPiece { transfer_id: "tid-1".into(), chunk: 3, chunk_len: 1000, offset: 256, hash: h, data: b"abcd".to_vec() });
        assert!(LinkPiece::decode(b"LCF1").is_none());
        assert!(LinkPiece::decode(&f[..20]).is_none());
        // an old peer's parser: the new link messages are just unknown and skipped
        let s = serde_json::to_string(&WsMsg::FileChunkAck { transfer_id: "t".into(), chunk: 1, error: None, gone: false, code: None }).unwrap();
        assert!(!s.contains("code"));
        assert_eq!(serde_json::from_str::<WsMsg>(&s).unwrap().link_reply_tid(), Some("t"));
        // an ack from an older receiver (no `code`) still parses
        let old = r#"{"type":"file_have","transfer_id":"t","have":[1],"complete":false}"#;
        assert!(matches!(serde_json::from_str::<WsMsg>(old).unwrap(), WsMsg::FileHave { code: None, .. }));
    }

    /// "I cannot connect to you" round-trips; to an older peer it is an unknown type, which its
    /// link loop skips (serde fails, the frame is ignored), never a protocol error.
    #[test]
    fn inbound_blocked_msg() {
        let m = WsMsg::InboundBlocked { addr: "192.168.3.214".into(), port: 47820 };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, r#"{"type":"inbound_blocked","addr":"192.168.3.214","port":47820}"#);
        assert_eq!(serde_json::from_str::<WsMsg>(&s).unwrap(), m);
        #[derive(serde::Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[allow(dead_code)]
        enum OldWsMsg {
            Ping,
            Pong,
        }
        assert!(serde_json::from_str::<OldWsMsg>(&s).is_err());
    }

    #[test]
    fn chunks() {
        assert_eq!(chunk_count(0, CHUNK_SIZE), 0);
        assert_eq!(chunk_count(1, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE + 1, CHUNK_SIZE), 2);
    }
}
