







use std::path::Path;
use std::sync::Arc;

use ferry_scan::{IgnorePolicy, ScanConfig, ScanEngine, StoreHandle};
use ferry_store::format::BlobId;
use ferry_store::manifest::{serialize_manifest, RootManifest};

use ferry_store::chunker::ValidatedPoly;

use crate::error::{CliError, CliResult};
use crate::folder::OpenFolder;


pub struct OneShot {
    pub manifest: RootManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_id: BlobId,
    pub stats: ferry_scan::walk::PassStats,
}


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



pub fn one_shot_raw(
    root: &Path,
    store: &Arc<ferry_store::store::Store>,
    poly: u64,
    folder_id: [u8; 16],
    device_id: [u8; 32],
    ignore: Arc<dyn IgnorePolicy>,
) -> CliResult<OneShot> {
    
    
    
    let poly = ValidatedPoly::try_from(poly).map_err(|e| {
        CliError::new(
            "poly-invalid",
            e.to_string(),
            "the folder's polynomial record is corrupt; restore this store from a known-good backup",
        )
    })?;
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
    
    drop(engine);
    Ok(OneShot {
        manifest_bytes: serialize_manifest(&manifest),
        manifest,
        manifest_id: current.manifest_id,
        stats,
    })
}

fn folder_rules(opened: &OpenFolder) -> CliResult<Arc<dyn IgnorePolicy>> {
    Ok(opened.ignore_policy())
}
