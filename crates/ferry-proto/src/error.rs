//! Typed protocol failures.
//!
//! Every abnormal session end maps to exactly one variant here. The BYE
//! reason codes on the wire (see `docs/store-format.md`, "Wire protocol v1")
//! are the coarse wire-visible projection of this enum; the typed error is
//! what callers match on.

use crate::version::ProtocolVersion;
use thiserror::Error;

/// Why a peer sent BYE, or why we send one. Wire values are normative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ByeReason {
    /// Clean, expected end of session.
    Normal = 0,
    /// Version negotiation found no workable common version.
    VersionIncompatible = 1,
    /// Framing, magic, message type, or state-machine violation.
    ProtocolViolation = 2,
    /// Handshake authentication failed: bad tag, wrong identity, replay.
    AuthFailed = 3,
    /// A resource limit was exceeded (oversized frame, counter exhausted).
    ResourceLimit = 4,
    /// Anything else; detail travels only in local logs, never on the wire.
    Internal = 5,
}

impl ByeReason {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ByeReason::Normal),
            1 => Some(ByeReason::VersionIncompatible),
            2 => Some(ByeReason::ProtocolViolation),
            3 => Some(ByeReason::AuthFailed),
            4 => Some(ByeReason::ResourceLimit),
            5 => Some(ByeReason::Internal),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ProtoError {
    #[error("transport i/o: {0}")]
    Io(#[from] std::io::Error),

    #[error(
        "version negotiation failed: we speak {ours}, they advertised {theirs}"
    )]
    VersionIncompatible {
        ours: ProtocolVersion,
        theirs: ProtocolVersion,
    },

    #[error("protocol violation: {0}")]
    ProtocolViolation(&'static str),

    #[error("unknown message type {msg_type:#04x} from a same-version peer")]
    UnknownMessage { msg_type: u8 },

    #[error("frame body of {len} bytes exceeds the {max} limit")]
    FrameTooLarge { len: usize, max: usize },

    #[error("authentication failed: {0}")]
    Auth(&'static str),

    #[error(
        "authenticated peer identity {got} does not match the expected peer {expected}"
    )]
    IdentityMismatch { expected: String, got: String },

    #[error("received blob failed verification: kind {kind:?} id {id} ({why})")]
    CorruptBlob {
        kind: ferry_store::format::BlobKind,
        id: String,
        why: &'static str,
    },

    #[error("{0} requested item(s) could not be served after retries")]
    MissingItems(usize),

    #[error("peer closed with BYE({reason:?})")]
    ByeReceived { reason: ByeReason },

    #[error("per-direction frame counter exhausted; session keys must be rekeyed (v1 has no rekey)")]
    CounterExhausted,

    #[error("folder {folder} is not shared by the peer")]
    FolderUnknown { folder: String },
}
