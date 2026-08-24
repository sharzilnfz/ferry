//! One-shot scans for CLI commands (status/sync/daemon rounds).
//!
//! Everything goes through ferry-scan's real pipeline (`ScanEngine`) so
//! ignore rules, `.ferry` structural exclusion, NFC normalization, and
//! refusal ledgers behave identically to the continuous daemon path. The
//! initial full scan completes synchronously before `watch_with` returns;
//! we snapshot `current()` and stop the background threads immediately.

use std::path::Path;
use std::sync::Arc;

use ferry_scan::{IgnorePolicy, ScanConfig, ScanEngine, StoreHandle};
use ferry_store::format::BlobId;
use ferry_store::manifest::{serialize_manifest, RootManifest};

use crate::error::{CliError, CliResult};
use crate::folder::OpenFolder;

/// The result of one policy-aware full scan.
pub struct OneShot {
    pub manifest: RootManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_id: BlobId,
    pub stats: ferry_scan::walk::PassStats,
}

/// Scan `opened.folder` once with its own rules and return the fresh state.
pub fn one_shot(opened: &OpenFolder, device_id: [u8; 32]) -> CliResult<OneShot> {
    let rules = folder_rules(opened)?;
    let out = one_shot_raw(
        &opened.root,
        &opened.store,
        opened.poly,
        opened.folder_id,
        device_id,
        rules,
    )?;
    Ok(out)
}

/// Same as [`one_shot`] but for callers that already hold compiled rules
/// (the exchange path reuses the daemon's compiled set).
pub fn one_shot_raw(
    root: &Path,
    store: &Arc<ferry_store::store::Store>,
    poly: u64,
    folder_id: [u8; 16],
    device_id: [u8; 32],
    ignore: Arc<dyn IgnorePolicy>,
) -> CliResult<OneShot> {
    let handle = StoreHandle {
        store: store.clone(),
        poly,
        folder_id,
        device_id,
    };
    let engine =
        ScanEngine::watch_with(root, handle, ScanConfig::default(), ignore).map_err(|e| {
            CliError::new(
                "scan",
                e.to_string(),
                "check the folder exists and is readable",
            )
        })?;
    let current = engine.current().ok_or_else(|| {
        CliError::new(
            "scan",
            "scanner produced no initial state",
            "retry the command",
        )
    })?;
    let manifest = current.manifest.clone();
    let stats = current.stats.clone();
    // Drop stops watcher/poller/auditor threads (Drop impl in ferry-scan).
    drop(engine);
    Ok(OneShot {
        manifest_bytes: serialize_manifest(&manifest),
        manifest,
        manifest_id: current.manifest_id,
        stats,
    })
}

fn folder_rules(opened: &OpenFolder) -> CliResult<Arc<dyn IgnorePolicy>> {
    Ok(Arc::new(crate::folder::load_rules(
        &opened.root,
        &opened.settings,
    )?))
}
