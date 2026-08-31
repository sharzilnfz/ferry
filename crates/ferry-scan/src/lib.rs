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
