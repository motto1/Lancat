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
    /// Reached through this peer (its name) because there is no direct path; None when direct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// A peer told us it cannot connect to us (`WsMsg::InboundBlocked`), or it had to send a file
/// over the link because of that: our inbound connections are blocked somewhere.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboundReport {
    pub peer_id: String,
    pub peer_name: String,
    /// Our address and port as the peer tried them.
    pub addr: String,
    pub port: u16,
    pub ts: i64,
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
    /// Something the user should fix on this device: `kind` = "inbound_blocked" (with `report`)
    /// or "recv_dir" (the receive folder cannot be written; `detail` says why).
    Alert {
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        report: Option<InboundReport>,
        #[serde(default)]
        detail: String,
    },
}
