//! Ferry store core: content-defined chunking, hash-addressed blobs, pack
//! files, indexes, manifests (serialization level), and pack-granularity GC.
//!
//! The byte-level contract for everything in this crate is
//! `docs/store-format.md`. ChaCha20-Poly1305 (ferry-crypto's `ChaChaCipher`)
//! is the pack cipher; the zero-crypto `PassthroughCipher` stub survives only
//! behind `cfg(test)` / the `test-util` feature for test fixtures. All
//! framing, salts, key schedule, nonces, and segment structure are fully
//! implemented and tested.

pub mod admission;
pub mod agreement;
pub mod chunker;
pub mod crypto;
pub mod diff;
pub mod format;
pub mod gc;
pub mod index;
pub mod manifest;
pub mod pack;
pub mod reclaim;
pub mod snapshot;
pub mod store;

pub use format::{BlobId, BlobKind, ContainerKind, PackId};
