//! Structured transfer failures: a machine-readable code next to the human text, so the UI can
//! say what went wrong and what fixes it (`FileBody::code`), and diagnostics can tell "the other
//! side's firewall" from "our receive folder" from "the network".

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrCode {
    /// The receiver is offline (or its address is unknown).
    PeerOffline,
    /// The receiver cannot be connected to and has no link to us: its firewall drops inbound
    /// connections (and it did not dial us either).
    PeerInboundBlocked,
    /// The receiver cannot be connected to and did not answer a transfer over its link: an older
    /// Lancat that cannot receive over the link, behind a firewall that blocks inbound connections.
    PeerOutdated,
    /// The receiver stopped answering in the middle of a transfer.
    PeerNoResponse,
    /// The connection to the receiver dropped during the transfer.
    LinkLost,
    /// The receiver could not write the file: its receive folder is not writable or its disk is full.
    PeerDirUnwritable,
    /// The file is larger than the receiver's size limit.
    PeerTooLarge,
    /// The receiver refused for another reason.
    PeerRejected,
    /// Receiving side: our receive folder is not writable (or the disk is full).
    LocalDirUnwritable,
    /// The file to send no longer exists.
    SourceMissing,
    /// The file to send could not be read.
    SourceUnreadable,
    /// The whole-file hash did not match twice.
    Verify,
    /// Some other network error.
    Network,
    Unknown,
}

impl ErrCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrCode::PeerOffline => "peer_offline",
            ErrCode::PeerInboundBlocked => "peer_inbound_blocked",
            ErrCode::PeerOutdated => "peer_outdated",
            ErrCode::PeerNoResponse => "peer_no_response",
            ErrCode::LinkLost => "link_lost",
            ErrCode::PeerDirUnwritable => "peer_dir_unwritable",
            ErrCode::PeerTooLarge => "peer_too_large",
            ErrCode::PeerRejected => "peer_rejected",
            ErrCode::LocalDirUnwritable => "local_dir_unwritable",
            ErrCode::SourceMissing => "source_missing",
            ErrCode::SourceUnreadable => "source_unreadable",
            ErrCode::Verify => "verify",
            ErrCode::Network => "network",
            ErrCode::Unknown => "unknown",
        }
    }

    /// The code a receiver puts in `FileHave` / `FileChunkAck` (link) for an error it answered
    /// over HTTP with `status`; the sender maps it back with [`ErrCode::from_peer`].
    pub fn wire_for_status(status: u16) -> Option<&'static str> {
        match status {
            507 => Some("dir_unwritable"),
            413 => Some("too_large"),
            _ => None,
        }
    }

    /// A receiver's refusal, from the HTTP status (or the link code).
    pub fn from_peer(status: Option<u16>, wire: Option<&str>) -> ErrCode {
        match (status, wire) {
            (Some(507), _) | (_, Some("dir_unwritable")) => ErrCode::PeerDirUnwritable,
            (Some(413), _) | (_, Some("too_large")) => ErrCode::PeerTooLarge,
            _ => ErrCode::PeerRejected,
        }
    }
}

/// An error with its code. Carried inside `anyhow::Error`; [`classify`] reads it back.
#[derive(Debug)]
pub struct TransferError {
    pub code: ErrCode,
    pub msg: String,
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for TransferError {}

pub fn terr(code: ErrCode, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(TransferError { code, msg: msg.into() })
}

/// The code of a failed transfer: an explicit [`TransferError`] anywhere in the chain, else a
/// guess from the error type (network errors, file errors).
pub fn classify(e: &anyhow::Error) -> ErrCode {
    for c in e.chain() {
        if let Some(t) = c.downcast_ref::<TransferError>() {
            return t.code;
        }
    }
    for c in e.chain() {
        if c.downcast_ref::<reqwest::Error>().is_some() {
            return ErrCode::Network;
        }
        // the sender's own file I/O (reading the source)
        if c.downcast_ref::<std::io::Error>().is_some() {
            return ErrCode::SourceUnreadable;
        }
    }
    ErrCode::Unknown
}

/// Errors that mean "this folder cannot be written": access denied, read-only, disk full.
pub fn is_storage_error(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    match e.raw_os_error() {
        // Windows: ERROR_ACCESS_DENIED, ERROR_WRITE_PROTECT, ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL
        #[cfg(windows)]
        Some(5 | 19 | 112 | 39) => true,
        // EACCES, EPERM, EROFS, ENOSPC, EDQUOT (Linux 122 / macOS 69)
        #[cfg(unix)]
        Some(13 | 1 | 30 | 28) => true,
        #[cfg(target_os = "linux")]
        Some(122) => true,
        #[cfg(target_os = "macos")]
        Some(69) => true,
        _ => false,
    }
}

/// An `anyhow` error caused by a storage error (see [`is_storage_error`]).
pub fn chain_is_storage_error(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<std::io::Error>().is_some_and(is_storage_error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        let e = terr(ErrCode::PeerInboundBlocked, "x").context("outer");
        assert_eq!(classify(&e), ErrCode::PeerInboundBlocked);
        assert_eq!(classify(&anyhow::anyhow!("plain")), ErrCode::Unknown);
        assert_eq!(ErrCode::from_peer(Some(507), None), ErrCode::PeerDirUnwritable);
        assert_eq!(ErrCode::from_peer(None, Some("too_large")), ErrCode::PeerTooLarge);
        assert_eq!(ErrCode::from_peer(Some(500), None), ErrCode::PeerRejected);
        assert_eq!(serde_json::to_string(&ErrCode::LocalDirUnwritable).unwrap(), "\"local_dir_unwritable\"");
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(chain_is_storage_error(&anyhow::Error::new(denied).context("open")));
    }
}
