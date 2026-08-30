//! # ferry-sync: the M0 walking skeleton
//!
//! Two daemons on one machine sync a directory **through the store**
//! (ADR-0001: stores and manifests are exchanged, never trees). Sessions
//! speak protocol v1 ([`session`] + [`exchange`]): device-key mutual auth,
//! ephemeral per-direction session keys, sealed frames, advert-driven
//! verified transfer, canonical last-agreed records. Encryption is ON by
//! default and authentication ALWAYS runs; the throwaway M0 plaintext
//! framing survives only behind [`EngineConfig::legacy_m0_proto`] (dev
//! flag, default OFF). T-009's transport swap rides underneath unchanged.
//!
//! ## Seams checklist (the actual M0 deliverable)
//!
//! Every interface this skeleton forced into existence, and what later
//! tickets slot into it without touching engine logic:
//!
//! 1. **[`transport::Transport`] / [`transport::Listener`] /
//!    [`transport::Connection`]** — byte-frame pipes. `TcpTransport`
//!    implements them with length-prefixed frames on localhost TCP;
//!    `ferry_iroh::IrohTransport` swaps in behind the same traits.
//!    Protocol v1 rides the seam via [`session::ConnLink`] (one frame body
//!    region per connection frame; the AEAD binds lengths either way),
//!    while [`session::RawLink`] speaks the literal u32 BE spec framing
//!    used by reference-interop tests.
//! 2. **Protocol** ([`session`], [`exchange`]) — the v1 inventory: mutual
//!    AUTH handshake (possession proofs, no signatures), version
//!    negotiation with BYE(1) on major mismatch, `FOLDER_OFFER` /
//!    `INDEX_ADVERT` rounds, `REQUEST_ITEMS/PACKS` → `ITEM_BATCH/PACK_ITEM`
//!    pulls, local last-agreed records, BYE; unknown-message policy per
//!    the normative skip-if-flagged rule. ([`proto`] holds the retired
//!    plaintext set behind the dev flag.)
//! 3. **Transfer granularity decision** — wanted chunks are grouped
//!    through the SERVER'S ADVERTISED index entries: whole packs by
//!    ciphertext name when ≥ 2 wanted chunks share a pack (receipt
//!    verifies `BLAKE3(ciphertext) == name` BEFORE anything touches disk),
//!    individual blobs otherwise; manifests/tree nodes always move as
//!    individual metadata blobs because the puller needs them before it
//!    can even compute what it wants. See [`exchange`].
//! 4. **[`ferry_materialize::Applier`]** — the applier boundary. Direct
//!    materialization with [`ferry_materialize::Applier::with_pin_gate`]:
//!    temp + atomic rename, parents-first creation, children-first deletion,
//!    exec bit, exact mtimes (files, symlinks AND directories, restored from
//!    the target tree), NFC live-name folding, and the untrusted-symlink-target
//!    policy that refuses hostile targets loudly. The puller applies the
//!    diff durably BEFORE round 2 observes equality — under v1, round 2
//!    plays AGREED's role.
//! 5. **Agreement bookkeeping** (`ferry_store::agreement::AgreementLedger`,
//!    the ONE canonical codec + ledger workspace-wide since T-10) — after
//!    each concluded session both sides record the agreed manifest id per
//!    peer in THE canonical 77-byte spec serialization (byte-exact on both
//!    sides). T-010's three-way reconciliation consumes these as base state.
//! 6. **Winner selection** — donor election collapsed into v1's symmetric
//!    pull stages: each side pulls what it lacks; adoption follows lineage
//!    (last-writer-wins) with M0's empty-vs-nonempty bootstrap guard
//!    intact, so single-direction flow and deterministic settlement
//!    survive without a donor message. `pick_donor`/`select_donor` remain
//!    exported for tests and future reuse. Simultaneous edits still LOSE
//!    the older writer's changes until T-010 ships quarantine.
//! 7. **Crypto insertion points** — the store is opened by ferry-folder
//!    (key unwrap + ChaCha20-Poly1305); this crate never names a cipher or a
//!    key. [`SyncEngine::with_store`] only accepts an already-opened store.
//!    Sessions seal every post-auth frame under ephemeral per-direction keys
//!    derived from device identity keys.
//!
//! ## What M0 deliberately does NOT do
//!
//! No watching (200 ms polling; T-004), no ignore rules (T-011), no
//! conflicts/quarantine (T-010), no cross-machine transports or pairing
//! (T-007/T-008/T-009), no Windows path handling beyond cfg-gated unix
//! extras (T-012).

pub mod engine;
pub mod exchange;
pub mod session;
pub mod transport;

// Re-exports so tests, bins, and future crates reach the vocabulary through
// one facade.
pub use engine::{
    pick_donor, select_donor, EngineConfig, EngineHandle, EngineStats, IngestError,
    PeerExpectation, PeerPolicy, PeerState, SessionError, SyncEngine,
};
pub use exchange::{ingest_pack_verified, run_v1_session, CurrentState, ExchangeHost};
pub use ferry_crypto::identity::DeviceIdentity;
pub use ferry_materialize::{ApplyOutcome, MaterializeError as ApplyError};
pub use ferry_store::format;
pub use ferry_store::{BlobId, BlobKind};
pub use session::{Established, ExpectPeer};
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
