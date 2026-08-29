//! `ferry store gc` (T-20): mark-from-live-manifests pack collection.
//!
//! The engine lives in ferry-store (`gc.rs`): `reachability_report` for the
//! read-only report, `collect_garbage` for the delete path behind it. This
//! module only gathers the LIVE ROOTS and renders the report:
//!
//! - every last-agreed manifest recorded for this folder
//!   (`.ferry/agreement/`, one per peer), and
//! - every held-change manifest still awaiting `ferry pin release`
//!   (`.ferry/held/<peer>.jsonl`), so releasing a pin can never hit a
//!   chunk whose pack GC already removed.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, SystemTime};

use ferry_store::agreement::AgreementLedger;
use ferry_store::format::{hex, unhex, BlobId};
use ferry_store::gc;

use crate::error::{CliError, CliResult, CodeInto};
use crate::folder;
use crate::out::Output;

pub struct GcArgs<'a> {
    pub folder: &'a Path,
    pub dry_run: bool,
    pub grace_secs: u64,
}

pub fn run(args: GcArgs<'_>) -> CliResult<Output> {
    let opened = folder::open_folder(args.folder)?;
    let roots = live_roots(&opened.root, &opened.folder_id)?;
    let now = SystemTime::now();

    if args.dry_run {
        let r = gc::reachability_report(&opened.store, &roots)
            .code("store", "reachability scan failed")?;
        let garbage: Vec<serde_json::Value> = r
            .garbage_packs
            .iter()
            .map(|(id, size)| serde_json::json!({ "pack": hex(id), "bytes": size }))
            .collect();
        Ok(Output::new(
            serde_json::json!({
                "command": "store",
                "action": "gc",
                "folder": args.folder.display().to_string(),
                "dry_run": true,
                "scanned_packs": r.scanned_packs,
                "live_packs": r.live_packs,
                "garbage_packs": garbage,
                "reclaimable_bytes": r.reclaimable_bytes,
                "skipped_corrupt": r.skipped_corrupt,
            }),
            format!(
                "{} packs scanned; {} live; {} unreachable holding {} bytes\n\
                 re-run without --dry-run and past --grace-secs to delete them\n",
                r.scanned_packs,
                r.live_packs,
                r.garbage_packs.len(),
                r.reclaimable_bytes,
            ),
        ))
    } else {
        let grace = Duration::from_secs(args.grace_secs);
        let r =
            gc::collect_garbage(&opened.store, &roots, grace, now).code("store", "gc failed")?;
        Ok(Output::new(
            serde_json::json!({
                "command": "store",
                "action": "gc",
                "folder": args.folder.display().to_string(),
                "dry_run": false,
                "scanned_packs": r.scanned,
                "deleted": r.deleted.iter().map(|id| hex(id)).collect::<Vec<_>>(),
                "recorded_unreferenced": r.recorded_unreferenced,
                "skipped_corrupt": r.skipped_corrupt,
            }),
            format!(
                "{} packs scanned; {} deleted; {} newly marked unreferenced (grace {}s)\n",
                r.scanned,
                r.deleted.len(),
                r.recorded_unreferenced,
                args.grace_secs,
            ),
        ))
    }
}

/// Live manifest roots for this folder's store: last-agreed pointers plus
/// held-change manifests. Sorted + deduped so reports are stable.
fn live_roots(folder_root: &Path, folder_id: &[u8; 16]) -> CliResult<Vec<BlobId>> {
    let state_dir = folder::state_dir(folder_root);
    let mut roots: BTreeSet<BlobId> = BTreeSet::new();
    let ledger = AgreementLedger::new(&state_dir);
    for (_, rec) in ledger.list_folder(folder_id).map_err(|e| {
        CliError::new(
            "agreement-state",
            e.to_string(),
            "check .ferry/agreement permissions",
        )
    })? {
        roots.insert(rec.manifest_id);
    }
    let held = ferry_pin::HeldLedger::new(&state_dir);
    for peer in held.peers().code("store", "held ledger unreadable")? {
        for e in held
            .load_peer(&peer)
            .code("store", "held ledger unreadable")?
        {
            if let Some(id) = unhex::<32>(&e.remote_manifest_id) {
                roots.insert(id);
            }
        }
    }
    Ok(roots.into_iter().collect())
}
