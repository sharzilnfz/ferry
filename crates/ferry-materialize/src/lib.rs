//! Materialization (T-005): apply manifests and change sets to a real
//! directory tree, atomically and crash-safely.
//!
//! Invariants upheld here (SPEC/ADR-0004):
//!
//! - A destination file is NEVER modified in place. Every write goes to a
//!   temp file in the destination's directory ([`temp`]) and is renamed
//!   into place; symlinks are created the same way. Kill -9 at any instant
//!   therefore leaves the old or the new state per path, never a torn file.
//! - Every chunk's hash is verified against its id after reading from the
//!   store (the store does this too — defense in depth) and AGAIN after
//!   writing the temp file, before any rename happens.
//! - The exec bit is authoritative from the manifest: set where the flag
//!   demands, cleared where it does not.
//! - No silent data loss: [`Overwrite::Expect`] refuses to touch any live
//!   path whose current state diverges from the caller's base expectation,
//!   returning every divergence instead of clobbering.

pub mod apply;
pub mod error;
pub mod temp;

pub use apply::resolve_live;
pub use apply::set_symlink_times;
pub use apply::{Applier, ApplyOutcome, ApplyStats, Overwrite, PinGate};
pub use error::{DivergeReason, Divergence, MaterializeError};
pub use temp::{sweep_stale_temps, TempStyle, DEFAULT_STALE_TEMP_AGE_SECS};
