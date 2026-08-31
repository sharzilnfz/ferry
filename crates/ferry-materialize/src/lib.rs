pub mod apply;
pub mod error;
pub mod temp;

pub use apply::resolve_live;
pub use apply::set_symlink_times;
pub use apply::{Applier, ApplyOutcome, ApplyStats, Overwrite};
pub use error::{DivergeReason, Divergence, MaterializeError};
pub use temp::{sweep_stale_temps, TempStyle, DEFAULT_STALE_TEMP_AGE_SECS};
