//! `ferry pin`: session pinning. Start/stop declare the active-writer
//! window; status surfaces the pin and its held set; release reconciles
//! every held change through ferry-sync-engine's three-way logic, so
//! outcomes are exactly ADR-0004 outcomes — winner live, loser quarantined,
//! entry in conflicts.jsonl. Nothing merges; nothing vanishes.
//!
//! `stop` ends the hold without reconciling but deliberately keeps the
//! ledgers: a later `release` still recovers them. Discarding held changes
//! is never an implicit side effect.

use std::fmt::Write as _;
use std::path::Path;

use ferry_pin::PinManager;
use ferry_store::format::hex;
use ferry_sync_engine::list_conflicts;
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder;
use crate::out::Output;

pub fn start(folder: &Path, paths: &[String], hours: u64) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let device_id = device_hex()?;
    let state_dir = opened.state_dir();

    let scope: Vec<String> = if paths.is_empty() {
        vec!["*".to_string()]
    } else {
        paths.to_vec()
    };

    // Try dispatching over IPC to running daemon first.
    let ipc_res = crate::ipc::send_command(
        folder,
        ferry_ipc::ClientCommand::StartPin {
            paths: scope.clone(),
            duration_hours: Some(hours),
        },
    );

    let (pid, started_sec, base_peers_recorded) = match ipc_res {
        Some(ferry_ipc::DaemonMessage::Ack { .. }) => {
            let pin_mgr = PinManager::new(&state_dir);
            if let Ok(Some(rec)) = pin_mgr.record() {
                (rec.pid, rec.started_sec, rec.base_agreements.len())
            } else {
                let (sec, _) = ferry_platform::time::now_unix();
                (std::process::id(), sec, 0)
            }
        }
        Some(ferry_ipc::DaemonMessage::Error { code, message }) => match code.as_str() {
            "bad-pattern" => {
                return Err(CliError::new(
                    "bad-pattern",
                    message,
                    "check the glob syntax (same rules as ferry.ignore)",
                ));
            }
            "pin-active" => {
                return Err(CliError::new(
                    "pin-active",
                    message,
                    "run `ferry pin stop` first (or `ferry pin status` to inspect)",
                ));
            }
            "pin_error" => {
                if message.contains("pattern")
                    || message.contains("syntax")
                    || message.contains("invalid")
                {
                    return Err(CliError::new(
                        "bad-pattern",
                        message,
                        "check the glob syntax (same rules as ferry.ignore)",
                    ));
                }
                return Err(CliError::new(
                    "pin-active",
                    message,
                    "run `ferry pin stop` first (or `ferry pin status` to inspect)",
                ));
            }
            _ => return Err(CliError::new("pin-error", message, "check pin state")),
        },
        _ => {
            return Err(CliError::new(
                "daemon-not-running",
                "no active background daemon is running for this folder",
                "start the background daemon with `ferry daemon` to enable session protection",
            ));
        }
    };

    let json_doc = json!({
        "command": "pin",
        "action": "start",
        "folder": opened.root.display().to_string(),
        "device_id": device_id,
        "pid": pid,
        "paths": scope,
        "started_at": ferry_platform::time::fmt_rfc3339(started_sec),
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
    let pin_mgr = PinManager::new(&state_dir);

    // Try dispatching over IPC to running daemon first.
    let ipc_res = crate::ipc::send_command(folder, ferry_ipc::ClientCommand::ReleasePin);
    let existed = match ipc_res {
        Some(ferry_ipc::DaemonMessage::Ack { message, .. }) => {
            message.as_deref() != Some("no active pin")
        }
        _ => pin_mgr.stop_session().map_err(pin_error)?,
    };

    // Surface what remains held so nobody mistakes stop for reconciliation.
    let summary = pin_mgr.summary().map_err(pin_error)?;
    let mut by_peer = serde_json::Map::new();
    for (peer, paths) in &summary.held_by_peer {
        by_peer.insert(peer.clone(), json!(paths.len()));
    }

    let json_doc = json!({
        "command": "pin",
        "action": "stop",
        "folder": opened.root.display().to_string(),
        "was_pinned": existed,
        "held_changes": summary.total_held_paths,
        "held_by_peer": by_peer,
    });
    let mut human = if existed {
        String::from("Unpinned   session ended; incoming edits apply again\n")
    } else {
        String::from("No pin     nothing was pinned here\n")
    };
    if summary.total_held_paths > 0 {
        let _ = writeln!(
            human,
            "Held       {} path(s) still ledgered — run `ferry pin release` to reconcile",
            summary.total_held_paths
        );
    }
    Ok(Output::new(json_doc, human))
}

pub fn release(folder: &Path) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let state_dir = opened.state_dir();
    let pin_mgr = PinManager::new(&state_dir);

    // Fresh scan: release reconciles the peer's held manifest against the
    // tree AS IT IS NOW (the apply half of earlier rounds may have landed).
    let scan = crate::commands::status::scan_now(&opened)?;

    let now = ferry_platform::time::now_unix();
    let mut peers = Vec::new();
    let mut total_quarantined = 0usize;
    let mut total_conflicts = 0usize;
    let mut total_ops = 0usize;
    for peer_hex in pin_mgr.held_peers().map_err(pin_error)? {
        // One transactional convergence per peer; the ledger clears only
        // after ITS convergence succeeded, so a failure leaves everything
        // retryable (re-running recomputes the same decisions).
        let rp = pin_mgr
            .release_peer(
                &peer_hex,
                &opened.store,
                &opened.root,
                &scan.manifest,
                None,
                now,
            )
            .map_err(pin_error)?;
        if rp.held_entries == 0 {
            continue;
        }
        pin_mgr.clear_peer(&peer_hex).map_err(pin_error)?;
        total_quarantined += rp.result.quarantined.len();
        total_conflicts += rp.result.conflicts.len();
        total_ops += rp.result.apply.mutations();
        peers.push(json!({
            "device_id": rp.device_id,
            "remote_manifest_id": rp.remote_manifest_id,
            "held_entries": rp.held_entries,
            "held_paths": rp.held_paths,
            "ops_applied": rp.result.apply.mutations(),
            "quarantined": rp.result.quarantined.len(),
            "conflicts_recorded": rp.result.conflicts.len(),
        }));
    }

    // End the marker too. Dispatch over IPC if daemon running, otherwise local mark_released.
    let ipc_res = crate::ipc::send_command(folder, ferry_ipc::ClientCommand::ReleasePin);
    let ended = match ipc_res {
        Some(ferry_ipc::DaemonMessage::Ack { .. }) => true,
        _ => pin_mgr.stop_session().map_err(pin_error)?,
    };

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

    let human = if peers.is_empty() {
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
    let pin_mgr = PinManager::new(&state_dir);
    let summary = pin_mgr.summary().map_err(pin_error)?;

    let started_at = summary.started_sec.map(ferry_platform::time::fmt_rfc3339);

    // Documented shape for `pin status`: peer → DISTINCT PATHS (the actual
    // held set), not counts.
    let mut held_by_peer = serde_json::Map::new();
    for (peer, list) in &summary.held_by_peer {
        held_by_peer.insert(peer.clone(), json!(list));
    }

    let json_doc = json!({
        "command": "pin",
        "action": "status",
        "folder": opened.root.display().to_string(),
        "state": summary.state,
        "device_id": summary.device_id,
        "pid": summary.pid,
        "started_at": started_at,
        "paths": summary.paths,
        "holding": summary.holding,
        "held_changes": summary.total_held_paths,
        "held_by_peer": held_by_peer,
    });

    let mut human = match summary.state.as_str() {
        "none" => {
            String::from("Pin        none — `ferry pin start [--paths <glob>...]` to begin\n")
        }
        s => format!("Pin        {s}\n"),
    };
    if !summary.paths.is_empty() {
        let _ = writeln!(human, "Scope      {}", summary.paths.join(", "));
    }
    if summary.total_held_paths == 0 {
        human.push_str("Held       nothing\n");
    } else {
        let _ = writeln!(human, "Held       {} path(s):", summary.total_held_paths);
        for (peer, list) in &summary.held_by_peer {
            let _ = writeln!(human, "  {}…", &peer[..8.min(peer.len())]);
            for p in list {
                let _ = writeln!(human, "    {p}");
            }
        }
        human.push_str("           `ferry pin release` reconciles these explicitly.\n");
    }

    Ok(Output::new(json_doc, human))
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
        E::Converge(_) => (
            "pin-release-reconcile",
            "three-way convergence failed during release; nothing was discarded",
        ),
        E::Store(_) | E::Io { .. } | E::Manifest(_) => ("store", "check .ferry permissions/disk"),
    };
    CliError::new(code, e.to_string(), hint)
}
