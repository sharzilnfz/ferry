//! T-015 session pinning: hold competing remote edits while one device is
//! the declared active writer, then converge through the ordinary three-way
//! engine on release (ADR-0004 layering — pinning never replaces quarantine).
//!
//! Module map:
//!
//! - [`pin`]: the `.ferry/pin-state.json` record and its store (start /
//!   mark-released / stale detection via pid liveness).
//! - [`matcher`]: compiled gitignore-style globs scoping the pin.
//! - [`held`]: the `.ferry/held/<peer>.jsonl` ledgers of held changes.
//! - [`hold`]: the hold seam — [`hold_matcher`] compiles the active pin's
//!   scope into the path gate the convergence engine applies, and
//!   [`record_held`] ledgers the engine's held decisions after a pinned
//!   convergence.
//! - [`release`]: rebuilds three-way inputs from the ledgers (base = last
//!   agreement captured at pin start) and executes the release through the
//!   transactional convergence engine; outcomes are exactly ADR-0004
//!   outcomes.

pub use ferry_sync_engine::held;
pub use ferry_sync_engine::hold;
pub use ferry_sync_engine::matcher;
pub use ferry_sync_engine::pin;
pub use ferry_sync_engine::pin_error as error;

pub mod manager;
pub mod release;

pub use ferry_sync_engine::pin_error::PinError;
pub use ferry_sync_engine::{
    distinct_paths, hold_matcher, record_held, HeldChunk, HeldEntry, HeldLedger, Liveness,
    PathMatcher, PinRecord, PinStore, PIN_FORMAT_VERSION,
};
pub use manager::{HeldSummary, PinManager, ReleasePeerResult, ReleaseSummary};
pub use release::{release_peer, ReleasePeerPlan};
