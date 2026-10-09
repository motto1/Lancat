//! Events pushed from the core to the UI (Tauri forwards them as `lancat://event`).

use serde::{Deserialize, Serialize};

use crate::store::{Message, Settings};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub os: String,
    pub ver: String,
    pub online: bool,
    pub addr: String,
    pub port: u16,
    pub blocked: bool,
    pub unread: i64,
    pub last_ts: i64,
    pub last_preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    Peer { peer: PeerView },
    PeerRemoved { peer_id: String },
    Message { message: Message },
    MessageUpdate { message: Message },
    Typing { peer_id: String, typing: bool },
    Progress { msg_id: String, peer_id: String, done: u64, total: u64, outgoing: bool },
    HistoryCleared { peer_id: Option<String> },
    Settings { settings: Settings },
}
