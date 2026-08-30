//! Tunables for one watched folder. Defaults follow the ticket: ~500 ms
//! debounce quiet window, 24 h audit cadence, 10 s poll fallback (Mutagen's
//! documented interval). Tests shrink everything to seconds or milliseconds.

use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanConfig {
    /// Write bursts coalesce into one scan pass after this much quiet.
    pub quiet_window: Duration,
    /// Full-hash audit cadence: re-hash every file and repair drift between
    /// disk and the last manifest (catches same-length rewrites whose mtime
    /// was restored behind the watcher).
    pub audit_interval: Duration,
    /// Poll fallback cadence for unwatchable subtrees (Linux descriptor
    /// exhaustion) and for root liveness checks everywhere else.
    pub poll_interval: Duration,
    /// Seed parent manifest id for the initial scan pass (e.g. from last agreement).
    pub parent_manifest_id: Option<ferry_store::BlobId>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        ScanConfig {
            quiet_window: Duration::from_millis(500),
            audit_interval: Duration::from_hours(24),
            poll_interval: Duration::from_secs(10),
            parent_manifest_id: None,
        }
    }
}
