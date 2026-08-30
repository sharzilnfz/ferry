//! Three-way reconciliation and conflict quarantine (T-010), fused into one
//! transactional convergence engine.
//!
//! The engine implements ADR-0004: every sync cycle is a three-way merge
//! between the local tree, the remote tree, and the last-agreed base
//! manifest (Mutagen's model). Divergent paths never auto-merge. The newer
//! side stays live; the loser's bytes survive as
//! `path.ferry-conflict.<loser-device-short>-<ts>` next to the winner, and a
//! structured entry lands in `.ferry/conflicts.jsonl`. Deletion versus edit
//! resurrects the edited version rather than letting it vanish.
//!
//! Module map:
//!
//! - [`converge`]: the [`converge::ConvergenceEngine`] — one call, one
//!   transactional unit. Three-way diff, blob fetch, atomic temp-file
//!   renames, conflict quarantine, `.ferry/conflicts.jsonl` appends, and
//!   the `AgreementLedger` commit happen inside it; callers see only the
//!   [`converge::ConvergenceResult`] and the filesystem.
//! - [`reconcile`]: the pure decision core inside that unit. Manifests in,
//!   internal plan out; no filesystem writes, and the plan type is
//!   crate-private — callers never see intermediate action plans.
//! - [`report`]: `conflicts.jsonl` append/read.
//! - [`naming`]: conflict-file names and collision handling.
//! - [`timefmt`]: fixed UTC time formatting used in names and reports.
//!
//! Decisions this crate owns (documented for review):
//!
//! - **Tiebreak.** Newer entry mtime wins. Exact mtime tie goes to the
//!   higher manifest device id (full 32-byte compare). Both devices compute
//!   the same winner because the comparison is symmetric.
//! - **Conflict name carries the LOSER's device short id** (first 8 hex
//!   chars), matching Syncthing, where the name records where the losing
//!   copy came from. The timestamp is the loser entry's own mtime, UTC,
//!   `YYYYMMDD-HHMMSS`: deterministic, so both sides derive identical
//!   expectations and tests do not depend on wall clocks.
//! - **Name collisions.** Candidates are tried in order `NAME`, `NAME-2`,
//!   `NAME-3`, ... against the live directory; the first absent name wins.
//!   There is no extension splitting: the conflict marker is a suffix on the
//!   full stored path, exactly as ADR-0004 writes it.
//! - **Quarantine files are ordinary files and therefore sync.** After the
//!   next exchange both devices hold the loser copy. Quarantined copies keep
//!   the loser's mtime and exec bit so the two sides converge to byte- and
//!   metadata-identical trees.
//! - **Deletion versus edit.** Any difference from base beats deletion, so
//!   even a metadata-only touch resurrects; the editor's version returns
//!   live at the original path with a report entry and no quarantine file
//!   (a deletion has no bytes to save).
//! - **Metadata-only divergence is silent.** Identical content that differs
//!   only in mtime or exec bit resolves deterministically (newer mtime,
//!   device tiebreak) without a conflict file: no bytes are at risk.
//! - **Directory paths do not conflict directly.** Both sides being dirs at
//!   one path is decided by their enumerated children; disjoint additions
//!   union cleanly.
//! - **Structural conflicts are loud.** One side removing or retyping an
//!   ancestor while the other changes something beneath it aborts with
//!   [`ReconcileError::StructuralConflict`] before any mutation. Quarantining
//!   a whole directory subtree is out of scope for v1.
//!
//! State location: conflicts.jsonl lives under the configured state dir
//! (the folder's `.ferry` in production); the agreement ledger lives under
//! the store directory. Tests keep both outside the synced trees because
//! the scanner does not yet exclude `.ferry`.
//!
//! ## Crate split: ferry-sync-engine vs ferry-sync
//!
//! This crate owns only convergence (pure three-way decision + transactional
//! materialization); `ferry-sync` owns the wire loop and transport. Merging the
//! two as spec.md:39 proposed would couple the pure decision core to the I/O
//! owning crate and prevent reuse from the offline release path. Split is
//! intentional for this milestone.

pub mod converge;
pub mod held;
pub mod hold;
pub mod matcher;
mod naming;
pub mod pin;
pub mod pin_error;
mod reconcile;
pub mod report;
#[cfg(test)]
pub(crate) mod testutil;

pub use converge::{
    converge, BlobFetch, ConvergenceEngine, ConvergenceError, ConvergenceResult, HeldDecision,
    HeldPath, LocalTree, Side,
};
pub use ferry_platform::time as timefmt;
pub use held::{distinct_paths, HeldChunk, HeldEntry, HeldLedger};
pub use hold::{hold_matcher, record_held};
pub use matcher::PathMatcher;
pub use naming::{conflict_display_name, device_short, unique_conflict_dest};
pub use pin::{Liveness, PinRecord, PinStore, PIN_FORMAT_VERSION};
pub use pin_error::PinError;
pub use reconcile::ReconcileError;
pub use report::{append_entries, list_conflicts, ConflictEntry, DeviceStamp, LogError};
