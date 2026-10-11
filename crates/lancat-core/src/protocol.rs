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
    fn chunks() {
        assert_eq!(chunk_count(0, CHUNK_SIZE), 0);
        assert_eq!(chunk_count(1, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE + 1, CHUNK_SIZE), 2);
    }
}
