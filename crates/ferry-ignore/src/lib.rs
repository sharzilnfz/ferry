//! Ignore rules and agent-state presets for Ferry (T-011): gitignore-syntax
//! selective rules, tuned defaults, and per-agent presets, plus a share-time
//! secret-scan heuristic.
//!
//! # Engine choice
//!
//! Pattern matching is delegated to the `ignore` crate (`BurntSushi`,
//! dual MIT/Apache-2.0 — compatible with this workspace) via
//! `ignore::gitignore::Gitignore::matched()`. That API compiles one rule
//! file's lines into an optimized glob set and answers single-path queries
//! with git-faithful semantics: glob `*?[]`, `**` doublestar, leading-`/`
//! anchoring, trailing-`/` dir-only, `!` negation, last-match-wins. What it
//! deliberately does NOT give us — its directory walker, which would rewalk
//! the tree and apply its own .gitignore discovery — we do not use:
//! ferry-scan owns walking (and event filtering), and this crate only ever
//! answers "is this relative path ignored?". Hand-rolling was rejected:
//! gitignore edge cases are notoriously subtle (anchoring vs. basename
//! matching, `literal_separator`, escaped classes), and `BurntSushi`'s
//! implementation is the de-facto reference used by ripgrep.
//!
//! # Layer precedence (defined precisely)
//!
//! Four ROOT-level layers are concatenated in order into one compiled
//! root chain; gitignore's own last-match-wins within that chain yields
//! exactly:
//!
//! ```text
//! built-in defaults  <  root ferry.ignore  <  applied presets  <  user overrides
//!      (lowest)                                                (user wins)
//! ```
//!
//! Per-directory rule files (`ferry.ignore`, and `.gitignore` when opted in)
//! in `SUBdirectories` stack after the whole root chain, shallowest first —
//! i.e. git's depth-first precedence: a deeper file's decisions override the
//! entire root chain *for paths beneath that directory*, including presets.
//! This mirrors git exactly (the root chain simply plays the role of the
//! top-level ignore file). Within one directory, `.gitignore` lines compile
//! first and `ferry.ignore` second, so Ferry-specific intent wins ties at
//! equal depth.
//!
//! Two exclusions sit OUTSIDE all layers by design:
//!
//! - Quarantine files (`*.ferry-conflict.*`) are NEVER ignorable — they must
//!   sync or conflict resolution loses data (ADR-0004).
//! - `.ferry/` (the store directory) is structurally hard-excluded by the
//!   scan walker itself; no rule can include it and none needs to.
//!
//! # Names and encodings
//!
//! Paths arrive as NFC-normalized UTF-8 components (the scan walker's
//! contract); pattern lines from rule files are NFC-normalized before
//! compilation so hand-written patterns match regardless of how the editor
//! encoded them. Matching joins components with `/`, never OS separators.

pub mod config;
pub mod defaults;
pub mod error;
pub mod policy;
pub mod presets;
pub mod secrets;

pub use config::IgnoreConfig;
pub use error::IgnoreError;
pub use policy::{is_quarantine_name, FerryIgnore};
pub use presets::Preset;
