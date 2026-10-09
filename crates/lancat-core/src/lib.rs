//! Lancat core: zero-config LAN sharing.
//!
//! * [`discovery`] — mDNS `_lancat._tcp.local.` + UDP 47820 broadcast heartbeat (5 s / 15 s offline)
//! * [`server`] / [`link`] / [`transfer`] — axum on TCP 47820: WebSocket messages, chunked resumable HTTP file upload
//! * [`store`] — SQLite devices / conversations / messages / settings
//! * [`node::Node`] — the facade the desktop app (Tauri) and the CLI drive
//!
//! The crate has no GUI dependency so it can be tested headless (`cargo test -p lancat-core`).

pub mod discovery;
pub mod events;
pub mod files;
mod link;
pub mod netutil;
pub mod node;
pub mod protocol;
pub mod server;
pub mod store;
pub mod transfer;

pub use events::{CoreEvent, PeerView};
pub use node::{Node, NodeConfig};
pub use protocol::{DeviceInfo, EmojiPayload};
pub use store::{FileBody, Message, Settings};
