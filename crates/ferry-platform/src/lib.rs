//! Cross-platform filesystem policy (T-012): the pure decision layer behind
//! every platform-specific guardrail, unit-tested identically on macOS,
//! Linux, and Windows.
//!
//! Policies decided here (each module documents its own rationale; ticket
//! T-012 records the decisions):
//!
//! - [`casefold`]: per-folder case-folding index. Two sibling entries whose
//!   names fold together cannot coexist on case-insensitive hosts (macOS,
//!   Windows). Scan and materialize refuse loudly naming both paths —
//!   Syncthing's five-year `casefs` saga is the cautionary tale
//!   (`research/landscape.md`, "Cross-platform pitfalls").
//! - [`winpath`]: Windows long-path handling via `\\?\` extended-length
//!   prefixes. Lifting MAX_PATH properly needs BOTH a registry value and an
//!   application manifest (Microsoft docs), neither of which a sync tool
//!   controls on the host — so paths are prefixed mechanically instead,
//!   regardless of host opt-in.
//! - [`reserved`]: Windows reserved device names (CON, PRN, AUX, NUL,
//!   COM1-9, LPT1-9, any extension). Policy: refuse loudly at scan and at
//!   materialize with a suggested rename; such entries could never exist on
//!   a Windows endpoint, so syncing them only guarantees a late surprise.
//! - [`links`]: symlink policy. Relative targets that stay inside the folder
//!   root sync as links; absolute targets and `..`-escaping targets are
//!   refused loudly (they would silently mean something different — or
//!   nothing — after materialization on another device). Directory links on
//!   Windows are refused unless the documented developer-mode escape hatch
//!   env var is set.

pub mod casefold;
pub mod links;
pub mod reserved;
pub mod time;
pub mod winpath;

pub use casefold::{find_case_conflict, fold_key, host_folds_case, CaseConflict, CaseFoldIndex};
pub use links::{allow_windows_dir_links, classify_link, LinkDecision, LinkRefusal};
pub use reserved::is_reserved_device_name;
pub use time::{join_unix, split_unix};
pub use winpath::{extend_path, needs_extended_length, MAX_PATH};
