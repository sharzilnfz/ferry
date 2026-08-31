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
    let parent = {
        let pin_rec = ferry_sync_engine::pin::PinStore::new(opened.state_dir())
            .load()
            .ok()
            .flatten();
        let base_from_pin = pin_rec.and_then(|r| {
            r.base_agreements
                .values()
                .next()
                .and_then(|h| ferry_store::format::unhex::<32>(h))
        });
        base_from_pin.or_else(|| {
            ferry_store::agreement::AgreementLedger::new(opened.state_dir())
                .list_folder(&opened.folder_id)
                .ok()
                .and_then(|recs| recs.into_iter().next().map(|(_, r)| r.manifest_id))
        })
    };
    let out = one_shot_raw_with_parent(
        &opened.root,
        &opened.store,
        opened.poly,
        opened.folder_id,
        device_id,
        rules,
        parent,
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
    one_shot_raw_with_parent(root, store, poly, folder_id, device_id, ignore, None)
}

pub fn one_shot_raw_with_parent(
    root: &Path,
    store: &Arc<ferry_store::store::Store>,
    poly: u64,
    folder_id: [u8; 16],
    device_id: [u8; 32],
    ignore: Arc<dyn IgnorePolicy>,
    parent_manifest_id: Option<BlobId>,
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
    let cfg = ScanConfig {
        parent_manifest_id,
        ..ScanConfig::default()
    };
    let engine = ScanEngine::watch_with(root, handle, cfg, ignore).map_err(|e| {
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
