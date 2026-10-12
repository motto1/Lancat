//! Lancat core: zero-config LAN sharing.
//!
//! * [`discovery`] — event-triggered UDP 47820 probe bursts (start / network change / wake / refresh) on
//!   every IPv4 interface incl. Tailscale peers, unicast replies, `bye` on exit; mDNS on physical LANs.
//!   Liveness = the per-peer WebSocket link (ping 30 s, offline after 2 missed)
//! * [`server`] / [`link`] / [`transfer`] — axum on TCP 47820: WebSocket messages, chunked resumable HTTP file upload
//! * [`store`] — SQLite devices / conversations / messages / settings
//! * [`node::Node`] — the facade the desktop app (Tauri) and the CLI drive
//!
//! The crate has no GUI dependency so it can be tested headless (`cargo test -p lancat-core`).

pub mod discovery;
pub mod errors;
pub mod events;
pub mod files;
mod link;
pub mod netutil;
pub mod node;
mod progress;
pub mod protocol;
pub mod server;
pub mod store;
pub mod transfer;

pub use errors::ErrCode;
pub use events::{CoreEvent, InboundReport, PeerView};
pub use node::{Node, NodeConfig};
pub use protocol::{DeviceInfo, EmojiPayload};
pub use store::{FileBody, Message, Settings};
