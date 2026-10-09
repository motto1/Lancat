//! Wire protocol shared by all Lancat peers.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1";
pub const SERVICE_TYPE: &str = "_lancat._tcp.local.";
pub const DEFAULT_PORT: u16 = 47820;
pub const CHUNK_SIZE: u64 = 4 * 1024 * 1024;
pub const HEARTBEAT_SECS: u64 = 5;
pub const OFFLINE_SECS: u64 = 15;
pub const HEADER_DEVICE_ID: &str = "x-lancat-id";
pub const HEADER_CHUNK_HASH: &str = "x-lancat-chunk-hash";

/// What a device says about itself (mDNS TXT, UDP heartbeat, WS hello, GET /info).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub os: String,
    pub ver: String,
    pub port: u16,
}

/// UDP broadcast heartbeat payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub lancat: u8,
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
    }

    #[test]
    fn chunks() {
        assert_eq!(chunk_count(0, CHUNK_SIZE), 0);
        assert_eq!(chunk_count(1, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE, CHUNK_SIZE), 1);
        assert_eq!(chunk_count(CHUNK_SIZE + 1, CHUNK_SIZE), 2);
    }
}
