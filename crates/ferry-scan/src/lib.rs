//! Incremental scan engine (T-004): filesystem watching, debounced
//! incremental manifests, overflow safety, poll fallback, periodic audits.
//!
//! The engine feeds `ferry-store`'s snapshot pipeline. Full scans are
//! literally [`ferry_store::snapshot::snapshot_dir`] runs (initial scan,
//! post-overflow recovery, scheduled audits); incremental passes rebuild only
//! dirty subtrees and splice them into the cached tree, with a size/mtime/exec
//! short-circuit so unchanged files are never read or re-hashed.
//!
//! # Correctness invariant
//!
//! After ANY event sequence — including queue overflow, watch-descriptor
//! exhaustion, or poll fallback — the next COMPLETED scan produces a manifest
//! equal to a from-scratch `snapshot_dir` result, modulo mtime-only changes on
//! entries whose content is unchanged ("mtime noise"; see `normalize`).
//! Overflow and audit paths guarantee this by construction (they ARE full
//! rescans); incremental passes are held to it by tests that compare against
//! a fresh snapshot oracle.
//!
//! # Per-OS policy matrix
//!
//! Watching follows Mutagen's documented posture (`research/landscape.md`,
//! [Mutagen watching docs](https://mutagen.io/documentation/synchronization/watching)):
//! native recursive events where the platform provides them, polling as the
//! safety net rather than the primary mechanism.
//!
//! | Platform | Native mechanism            | Recursion   | Loss signal                          | Response in this crate                                              |
//! |----------|-----------------------------|-------------|--------------------------------------|---------------------------------------------------------------------|
//! | macOS    | `FSEvents`                    | native      | coalescing/history drops surface as synthetic events or errors | unclassifiable loss => full rescan; root liveness checked on every poll tick |
//! | Linux    | inotify (one watch per dir) | notify adds per-directory watches | `IN_Q_OVERFLOW` (queue) or `ENOSPC`/`EMFILE`/`ENFILE` from watch registration | queue overflow => full rescan; registration failure => mark that subtree **unwatchable** and start poll fallback at `ScanConfig::poll_interval` (default 10 s) |
//! | Windows  | `ReadDirectoryChangesW`       | native      | fixed buffer overrun (`ERROR_NOTIFY_ENUM_DIR`) discards events | surfaces as an error/overflow marker => full rescan                  |
//!
//! Two rules hold everywhere:
//!
//! 1. **Correctness over precision.** Any watcher error this crate cannot
//!    confidently classify as benign is treated as event loss and triggers a
//!    full rescan. A redundant rescan costs seconds of hashing; a lost event
//!    silently desyncs peers.
//! 2. **Polling is per-subtree, not global.** Only subtrees whose watches
//!    could not be established fall back to polling; the rest keep native
//!    latency. Poll passes are stat-only sweeps (no hashing); mismatches feed
//!    the same dirty-subtree machinery as native events, so a polled subtree
//!    converges to exactly the same manifest state.
//!
//! The decision layer is pure and unit-tested without kernel involvement:
//! see [`policy::PolicyState`] consuming [`policy::WatchSignal`] values and
//! returning [`policy::Action`]s. The platform glue in `engine` only maps
//! notify errors/events onto those signals.
//!
//! # Threading model
//!
//! std threads only, no async runtime: one notify watcher thread, one poll
//! thread, one audit timer thread, one worker draining a shared signal queue,
//! plus caller threads entering [`ScanEngine::scan_once`]. Passes serialize on
//! a mutex around the engine core; whoever drains the queue runs the work.
//!
//! # Structural exclusion
//!
//! The store directory itself (`.ferry`, per `docs/store-format.md`) is always
//! excluded from scans and watches. That is not an ignore rule: it is part of
//! the folder layout contract, so it is hard-coded here rather than routed
//! through [`IgnorePolicy`] (which is user-facing and lands fully in T-011).

pub mod config;
pub mod engine;
pub mod error;
pub mod ignore;
pub mod normalize;
pub mod policy;
pub mod state;
#[cfg(test)]
pub(crate) mod testutil;
pub mod walk;

pub use config::ScanConfig;
pub use engine::{CurrentScan, ScanEngine, ScanEvent, ScanRun, StoreHandle};
pub use error::ScanError;
pub use ignore::{EntryKind, IgnorePolicy, NoIgnores};
pub use normalize::{canonical_tree_id, equivalent_modulo_mtime};
pub use policy::{Action, PolicyState, RelPath, Trigger, WatchSignal};
pub use walk::{PassStats, ScanOutput};
