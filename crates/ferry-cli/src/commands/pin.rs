//! `ferry pin`: session pinning. Start/stop declare the active-writer
//! window; status surfaces the pin and its held set; release reconciles
//! every held change through ferry-sync-engine's three-way logic, so
//! outcomes are exactly ADR-0004 outcomes — winner live, loser quarantined,
//! entry in conflicts.jsonl. Nothing merges; nothing vanishes.
//!
//! `stop` ends the hold without reconciling but deliberately keeps the
//! ledgers: a later `release` still recovers them. Discarding held changes
//! is never an implicit side effect.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use ferry_pin::{plan_release, HeldEntry, HeldLedger, PathMatcher, PinRecord, PinStore};
use ferry_store::format::hex;
use ferry_sync_engine::{list_conflicts, PeerState};
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder;
use crate::out::Output;

pub fn start(folder: &Path, paths: &[String]) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let device_id = device_hex()?;
    let state_dir = opened.state_dir();

    // Validate BEFORE writing anything: a stored pin must always compile.
    if let Err(e) = PathMatcher::new(paths) {
        return Err(pin_error(e));
    }
    let scope: Vec<String> = if paths.is_empty() {
        vec!["*".to_string()]
    } else {
        paths.to_vec()
    };

    // Freeze the last-agreed base per peer NOW: release's three-way
    // ancestor is exactly "last agreed before the pin started".
    let ps = PeerState::new(&state_dir);
    let mut base_agreements = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(state_dir.join("peers")) {
        for path in rd.flatten().map(|e| e.path()) {
            if let Some(dev) = ferry_sync_engine::agree::peer_from_path(&path) {
                if let Ok(Some(rec)) = ps.load(&dev) {
                    base_agreements.insert(hex(&dev), hex(&rec.manifest_id));
                }
            }
        }
    }

    let (sec, nsec) = ferry_sync_engine::timefmt::now_unix();
    let pid = std::process::id();
    let base_peers_recorded = base_agreements.len();
    let store = PinStore::new(&state_dir);
    store
        .start(&PinRecord {
            format_version: ferry_pin::PIN_FORMAT_VERSION,
            device_id: device_id.clone(),
            pid,
            started_sec: sec,
            started_nsec: nsec,
            paths: scope.clone(),
            released: false,
            base_agreements,
        })
        .map_err(pin_error)?;

    let json_doc = json!({
        "command": "pin",
        "action": "start",
        "folder": opened.root.display().to_string(),
        "device_id": device_id,
        "pid": pid,
        "paths": scope,
        "started_at": ferry_sync_engine::timefmt::fmt_rfc3339(sec),
        "base_peers_recorded": base_peers_recorded,
    });

    let human = format!(
        "Pinned     {} (pid {pid})\nScope      {}\nHolds      remote edits to these paths until `ferry pin release`\n",
        opened.root.display(),
        scope.join(", ")
    );
    Ok(Output::new(json_doc, human))
}

pub fn stop(folder: &Path) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let state_dir = opened.state_dir();
    let store = PinStore::new(&state_dir);
    let existed = store.mark_released().map_err(pin_error)?;

    // Surface what remains held so nobody mistakes stop for reconciliation.
    let held = held_summary(&state_dir)?;

    let json_doc = json!({
        "command": "pin",
        "action": "stop",
        "folder": opened.root.display().to_string(),
        "was_pinned": existed,
        "held_changes": held.total_paths,
        "held_by_peer": held.by_peer,
    });
    let mut human = if existed {
        String::from("Unpinned   session ended; incoming edits apply again\n")
    } else {
        String::from("No pin     nothing was pinned here\n")
    };
    if held.total_paths > 0 {
        let _ = writeln!(
            human,
            "Held       {} path(s) still ledgered — run `ferry pin release` to reconcile",
            held.total_paths
        );
    }
    Ok(Output::new(json_doc, human))
}

pub fn release(folder: &Path) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let state_dir = opened.state_dir();

    let bases = match PinStore::new(&state_dir).load().map_err(pin_error)? {
        Some(rec) => rec.base_agreements,
        None => BTreeMap::new(),
    };

    // Fresh scan: release reconciles the peer's held manifest against the
    // tree AS IT IS NOW (the apply half of earlier rounds may have landed).
    let scan = crate::commands::status::scan_now(&opened)?;
    let ledger = HeldLedger::new(&state_dir);

    let plans = plan_release(&opened.store, &scan.manifest, &bases, &ledger).map_err(pin_error)?;

    let now = ferry_sync_engine::timefmt::now_unix();
    let mut peers = Vec::with_capacity(plans.len());
    let mut total_quarantined = 0usize;
    let mut total_conflicts = 0usize;
    let mut total_ops = 0usize;
    for rp in &plans {
        let stats = ferry_sync_engine::execute(
            &opened.store,
            &opened.root,
            &rp.plan,
            Some(&state_dir),
            now,
        )
        .map_err(|e| cli("pin-release-execute", e))?;
        // Clear ONLY after this plan executed: a failure leaves everything
        // retryable (re-running recomputes the same decisions).
        ledger.clear_peer(&rp.device_id).map_err(pin_error)?;
        total_quarantined += stats.quarantined.len();
        total_conflicts += stats.conflicts.len();
        total_ops += stats.apply.mutations();
        peers.push(json!({
            "device_id": rp.device_id,
            "remote_manifest_id": rp.remote_manifest_id,
            "held_entries": rp.held_entries,
            "held_paths": rp.held_paths,
            "ops_applied": stats.apply.mutations(),
            "quarantined": stats.quarantined.len(),
            "conflicts_recorded": stats.conflicts.len(),
        }));
    }

    // End the marker too (absent/released/stale pins are all fine here).
    let ended = PinStore::new(&state_dir)
        .mark_released()
        .map_err(pin_error)?;

    let conflicts_total = list_conflicts(&state_dir)
        .map(|e| e.len())
        .map_err(|e| cli("conflict-log", e))
        .unwrap_or(total_conflicts);

    let json_doc = json!({
        "command": "pin",
        "action": "release",
        "folder": opened.root.display().to_string(),
        "peers": peers,
        "quarantined": total_quarantined,
        "conflicts_recorded": total_conflicts,
        "ops_applied": total_ops,
        "pin_ended": ended,
        "conflicts_total": conflicts_total,
    });

    let human = if plans.is_empty() {
        String::from("Release    nothing held — no-op\n")
    } else {
        let mut h = String::new();
        for p in &peers {
            let _ = writeln!(
                h,
                "Released   peer {}… held={} quarantined={} conflict(s)={}",
                p["device_id"].as_str().unwrap().get(..8).unwrap_or(""),
                p["held_entries"],
                p["quarantined"],
                p["conflicts_recorded"],
            );
        }
        let _ = writeln!(
            h,
            "Total      {total_quarantined} loser copy/copies, {total_conflicts} conflict entr(y/ies) in conflicts.jsonl"
        );
        h
    };

    Ok(Output::new(json_doc, human))
}

pub fn status(folder: &Path) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let state_dir = opened.state_dir();
    let record = PinStore::new(&state_dir).load().map_err(pin_error)?;

    let (state, paths, device_id, pid, started_at): (
        &str,
        Vec<String>,
        Option<String>,
        Option<u32>,
        Option<String>,
    ) = match &record {
        None => ("none", Vec::new(), None, None, None),
        Some(rec) => {
            let s = if rec.released {
                "released"
            } else if rec.holding() {
                "active"
            } else {
                "stale"
            };
            (
                s,
                rec.paths.clone(),
                Some(rec.device_id.clone()),
                Some(rec.pid),
                Some(ferry_sync_engine::timefmt::fmt_rfc3339(rec.started_sec)),
            )
        }
    };

    let held = held_summary(&state_dir)?;

    // Documented shape for `pin status`: peer → DISTINCT PATHS (the actual
    // held set), not counts.
    let mut held_by_peer = serde_json::Map::new();
    for (peer, list) in &held.by_peer_list {
        held_by_peer.insert(peer.clone(), json!(list));
    }

    let json_doc = json!({
        "command": "pin",
        "action": "status",
        "folder": opened.root.display().to_string(),
        "state": state,
        "device_id": device_id,
        "pid": pid,
        "started_at": started_at,
        "paths": paths,
        "holding": record.as_ref().is_some_and(ferry_pin::PinRecord::holding),
        "held_changes": held.total_paths,
        "held_by_peer": held_by_peer,
    });

    let mut human = match state {
        "none" => {
            String::from("Pin        none — `ferry pin start [--paths <glob>...]` to begin\n")
        }
        s => format!("Pin        {s}\n"),
    };
    if !json_doc["paths"].as_array().unwrap().is_empty() {
        let _ = writeln!(
            human,
            "Scope      {}",
            json_doc["paths"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if held.total_paths == 0 {
        human.push_str("Held       nothing\n");
    } else {
        let _ = writeln!(human, "Held       {} path(s):", held.total_paths);
        for (peer, list) in &held.by_peer_list {
            let _ = writeln!(human, "  {}…", &peer[..8.min(peer.len())]);
            for p in list {
                let _ = writeln!(human, "    {p}");
            }
        }
        human.push_str("           `ferry pin release` reconciles these explicitly.\n");
    }

    Ok(Output::new(json_doc, human))
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

struct HeldSummary {
    /// Distinct held paths across every peer.
    total_paths: usize,
    /// peer hex → distinct path count (JSON-friendly).
    by_peer: serde_json::Map<String, serde_json::Value>,
    /// Sorted (peer, sorted distinct paths) pairs for human rendering.
    by_peer_list: Vec<(String, Vec<String>)>,
}

fn held_summary(state_dir: &Path) -> CliResult<HeldSummary> {
    let ledger = HeldLedger::new(state_dir);
    let mut by_peer = serde_json::Map::new();
    let mut by_peer_list = Vec::new();
    let mut total = 0usize;
    for peer in ledger.peers().map_err(pin_error)? {
        let entries: Vec<HeldEntry> = ledger.load_peer(&peer).map_err(pin_error)?;
        let paths = ferry_pin::distinct_paths(&entries);
        total += paths.len();
        by_peer.insert(peer.clone(), json!(paths.len()));
        by_peer_list.push((peer, paths));
    }
    by_peer_list.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(HeldSummary {
        total_paths: total,
        by_peer,
        by_peer_list,
    })
}

fn device_hex() -> CliResult<String> {
    let home = crate::home::ferry_home()?;
    let identity = ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
        .map_err(|e| {
            CliError::new(
                "identity-corrupt",
                e.to_string(),
                "restore or replace your device.key",
            )
        })?;
    Ok(hex(identity.public()))
}

fn cli(code: &'static str, e: impl std::fmt::Display) -> CliError {
    match code {
        "conflict-log" => {
            CliError::new(code, e.to_string(), "fix or archive .ferry/conflicts.jsonl")
        }
        _ => CliError::new(
            code,
            e.to_string(),
            "retry `ferry pin release`; nothing was discarded",
        ),
    }
}

/// Map typed pin errors onto stable CLI error codes with honest hints.
fn pin_error(e: ferry_pin::PinError) -> CliError {
    use ferry_pin::PinError as E;
    let (code, hint) = match &e {
        E::PinActive { .. } => (
            "pin-active",
            "run `ferry pin stop` first (or `ferry pin status` to inspect)",
        ),
        E::BadPattern { .. } => (
            "bad-pattern",
            "check the glob syntax (same rules as ferry.ignore)",
        ),
        E::Corrupt { .. } => (
            "pin-state-corrupt",
            "fix or delete .ferry/pin-state.json (it is small and safe to inspect)",
        ),
        E::LedgerCorrupt { .. } => (
            "held-ledger-corrupt",
            "the held set is damaged; entries before the bad line are recoverable via `ferry pin release`",
        ),
        E::ManifestMissing { .. } => (
            "held-manifest-missing",
            "the held change's manifest left the store; release cannot reconstruct it safely",
        ),
        E::StructuralSplit { .. } => (
            "structural-split",
            "widen or narrow --paths so pinned and unpinned changes do not nest",
        ),
        E::Reconcile(_) => (
            "pin-release-reconcile",
            "three-way reconcile failed during release; nothing was discarded",
        ),
        E::Store(_) | E::Io { .. } | E::Manifest(_) => ("store", "check .ferry permissions/disk"),
    };
    CliError::new(code, e.to_string(), hint)
}
