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
//! - [`split`]: the hold filter — partitions an [`ActionPlan`] into an
//!   apply-now half and a held half; [`hold_filter`] is the seam the
//!   exchange loop consults pre-apply.
//! - [`release`]: rebuilds three-way reconcile inputs from the ledgers
//!   (base = last agreement captured at pin start) and returns executable
//!   plans; outcomes are exactly ADR-0004 outcomes.

pub mod error;
pub mod held;
pub mod matcher;
pub mod pin;
pub mod release;
pub mod split;

pub use error::PinError;
pub use held::{distinct_paths, HeldChunk, HeldEntry, HeldLedger};
pub use matcher::PathMatcher;
pub use pin::{Liveness, PinRecord, PinStore, PIN_FORMAT_VERSION};
pub use release::{plan_release, ReleasePeerPlan};
pub use split::{hold_filter, split_plan, HoldDecision, SplitPlan};
