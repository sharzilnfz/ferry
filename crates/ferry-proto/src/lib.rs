//! Ferry wire protocol v1 (ticket T-008): framing, version negotiation,
//! mutual device-key authentication, folder advertisement, verified blob
//! transfer, and agreement bookkeeping.
//!
//! The byte-level contract is the "Wire protocol v1" section of
//! `docs/store-format.md`. Everything that document already defines moves
//! byte-for-byte: manifests and tree nodes transfer exactly as stored, index
//! entries become advertisement rows in the index-table serialization, packs
//! move whole under their ciphertext names. This crate owns only the
//! transport-shaped parts the format spec deferred: frames, handshake,
//! session keys, request/response flow.
//!
//! # Module map
//!
//! - [`error`]: typed protocol failures; every rejection is one of these.
//! - [`version`]: `ProtocolVersion` (major.minor in a u16) and the
//!   negotiation rule.
//! - [`stream`]: the [`ByteStream`](stream::ByteStream) abstraction over any
//!   byte pipe (TCP today, relay pipes later) plus an in-memory duplex pair
//!   for loopback harnesses.
//! - [`frame`]: length-prefixed frames carrying magic, message type, and
//!   protocol version.
//! - [`codec`]: message inventory — exact payload layouts for every message
//!   type on the wire.
//! - [`secure`]: handshake key schedule, transcript, per-direction traffic
//!   keys, and the AEAD seal/open layer applied to frames after auth.
//! - [`engine`]: the conversation driver: hello → authenticate → offers →
//!   pull → re-offer → agree → bye. Agreement records themselves live in
//!   `ferry_store::agreement` (the single canonical codec + ledger).
//!
//! # Security posture
//!
//! Session keys are ephemeral-per-connection and forward-secret by
//! construction: three X25519 shared secrets (ephemeral-ephemeral, each
//! static against the peer's ephemeral) feed one HKDF chain rooted in a hash
//! of the full transcript. Each side's possession of its static secret is
//! proven implicitly — an attacker without it cannot produce the single
//! AEAD-sealed auth message keyed through that term. Post-auth frames are
//! sealed with ChaCha20-Poly1305 under per-direction keys with strictly
//! increasing counter nonces; replayed, reordered, or tampered frames fail
//! authentication and drop the connection. See `docs/adr/0002` (no plaintext
//! leaves the process) and `docs/adr/0003` (peers are their public keys).

pub mod codec;
pub mod engine;
pub mod error;
pub mod frame;
pub mod secure;
pub mod stream;
pub mod version;

#[cfg(test)]
mod engine_tests;

pub use engine::{run_engine, EngineConfig, FolderState, Granularity, Role, SessionReport};
pub use error::ProtoError;
pub use ferry_crypto::identity::DeviceId;
pub use secure::SecureSession;
pub use stream::{duplex_pair, ByteStream};
pub use version::ProtocolVersion;

/// The magic starting every pre-auth frame body region: "FRW1".
///
/// Distinct from container files ("FERRY"): a wire peer and a file reader
/// must never be able to confuse their inputs.
pub const WIRE_MAGIC: [u8; 4] = *b"FRW1";
