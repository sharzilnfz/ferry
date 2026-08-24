//! # ferry-sync: the M0 walking skeleton
//!
//! Two daemons on one machine sync a directory **through the store** over
//! plain localhost TCP (ADR-0001: stores and manifests are exchanged, never
//! trees). Deliberately throwaway parts: the TCP transport (T-008/T-009 own
//! the real iroh QUIC transport), encryption (OFF; T-007/T-008 insert it),
//! and the inline materializer (T-005 replaces it). Its job is to prove
//! store → manifest diff → transfer → materialize END-TO-END and to force
//! every interface seam into existence early.
//!
//! ## Seams checklist (the actual M0 deliverable)
//!
//! Every interface this skeleton forced into existence, and what later
//! tickets slot into it without touching engine logic:
//!
//! 1. **[`transport::Transport`] / [`transport::Listener`] /
//!    [`transport::Connection`]** — byte-frame pipes. `TcpTransport`
//!    implements them with length-prefixed frames on localhost TCP. T-009's
//!    real transport swaps in here; T-008's protocol rides on the same
//!    messages ([`proto`]).
//! 2. **Protocol message inventory** ([`proto`]) — hello, offer, requests,
//!    items, agreement, error. Throwaway framing/flow for M0; T-008 owns the
//!    durable protocol but must reuse these serializations' shape: manifests
//!    move exactly as stored, packs move whole by ciphertext name.
//! 3. **Transfer granularity decision** — data chunks are requested BY ID
//!    and served as WHOLE PACKS by ciphertext name when a needed chunk lives
//!    there (verified on receipt via `BLAKE3(ciphertext) == name`); tree
//!    nodes and manifests move as individual metadata blobs. Rationale:
//!    pack-granular transfer matches the spec wire note ("whole packs
//!    transfer as units named by ciphertext hash so end-to-end integrity
//!    checking costs nothing extra") and exercises the exact unit T-008
//!    will resume/chunk; meta blobs stay individual because the puller needs
//!    them before it can even compute what it wants. See `engine.rs`.
//! 4. **[`materialize::Materializer`] + [`materialize::BlobSource`]** — the
//!    applier seam. The inline implementation is ugly-but-correct (temp +
//!    atomic rename, parents-first creation, children-first deletion, exec
//!    bit, exact mtimes incl. symlink times). T-005's real applier
//!    implements the same trait against the same change-set inputs.
//! 5. **Agreement bookkeeping** ([`state::AgreementStore`]) — after each
//!    concluded session both sides record the agreed manifest id per peer in
//!    the spec-shaped last-agreed record (`docs/store-format.md`,
//!    "Last-agreed manifest pointer"). Today it is written and read as the
//!    sync baseline; T-010's three-way reconciliation consumes it as base
//!    state.
//! 6. **Winner selection** ([`engine::pick_donor`]) — deterministic
//!    donor/puller choice from two exchanged manifests. M0 rule: empty vs
//!    non-empty prefers non-empty (safe bootstrap); otherwise last-writer-
//!    wins by manifest lineage. Simultaneous edits therefore LOSE DATA by
//!    design until T-010 ships quarantine.
//! 7. **Crypto insertion points** — the store cipher is v0 pass-through
//!    (`ferry_store::crypto::PassthroughCipher`) and no frame is encrypted;
//!    T-007/T-008 wrap key material around [`state`] records and every
//!    [`proto`] payload without changing flow control.
//!
//! ## What M0 deliberately does NOT do
//!
//! No watching (200 ms polling; T-004), no ignore rules (T-011), no
//! conflicts/quarantine (T-010), no cross-machine transports or pairing
//! (T-007/T-008/T-009), no Windows path handling beyond cfg-gated unix
//! extras (T-012).

pub mod engine;
pub mod materialize;
pub mod proto;
pub mod session;
pub mod state;
pub mod transport;

// Re-exports so tests, bins, and future crates reach the vocabulary through
// one facade.
pub use engine::{
    pick_donor, EngineConfig, EngineHandle, EngineStats, IngestError, SessionError, SyncEngine,
};
pub use ferry_store::format;
pub use ferry_store::{BlobId, BlobKind};
pub use materialize::{BlobSource, InlineMaterializer, MaterializeError, Materializer};
pub use state::{device_id_from_tag, AgreementStore};
pub use transport::{Connection, Listener, TcpTransport, Transport};

/// M0's default folder id (both daemons must share one folder id; the real
/// init ritual arrives with T-007).
pub const DEFAULT_FOLDER_ID: [u8; 16] = [
    0x6d, 0x30, 0x2d, 0x73, 0x6b, 0x65, 0x6c, 0x3a, // "m0-skel:"
    0x66, 0x6f, 0x6c, 0x64, 0x65, 0x72, 0x21, 0x21, // "folder!!"
];

/// Empty-directory tree id: BLAKE3 of the canonical empty tree node. Used
/// by donor selection to detect the bootstrap case without touching a store.
pub fn empty_tree_id() -> BlobId {
    *blake3::hash(&ferry_store::manifest::serialize_tree_node(
        &ferry_store::manifest::TreeNode {
            entries: Vec::new(),
        },
    ))
    .as_bytes()
}
