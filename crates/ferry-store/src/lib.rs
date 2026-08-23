//! Ferry store core: content-defined chunking, hash-addressed blobs, pack
//! files, indexes, manifests (serialization level), and pack-granularity GC.
//!
//! The byte-level contract for everything in this crate is
//! `docs/store-format.md` at the repository root. One deliberate deviation is
//! compiled in right now: the pack cipher is a pass-through stub
//! ([`crypto::PassthroughCipher`]) until T-007/T-008 wire real keys. All
//! framing, salts, key schedule, nonces, and segment structure are fully
//! implemented and tested, so swapping the real ChaCha20-Poly1305 in touches
//! only `crypto.rs`.

pub mod chunker;
pub mod crypto;
pub mod format;
pub mod gc;
pub mod index;
pub mod manifest;
pub mod pack;
pub mod store;

pub use format::{BlobId, BlobKind, ContainerKind, PackId};
