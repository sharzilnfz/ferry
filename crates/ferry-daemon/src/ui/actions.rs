use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{json, Value};

use ferry_folder::folder::{load_rules, open_folder};
use ferry_folder::pairing::{accept_begin, accept_complete, initiate_begin, initiate_complete};
use ferry_pin::{PinError, PinManager};
use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex as hex_str;

use super::{OpError, UiState};

const PAIR_TIMEOUT_SECS: u64 = 120;

pub(super) fn pin_manager(st: &UiState) -> PinManager {
    PinManager::new(st.state_dir())
}

pub(super) fn pin_err(e: PinError) -> OpError {
    match e {
        PinError::PinActive { pid } => OpError::new(
            "pin-active",
            format!("a pinned session (pid {pid}) already holds this folder"),
            "stop or release it first",
        ),
        PinError::Corrupt { path, reason } => OpError::new(
            "pin-state-corrupt",
            format!("{}: {reason}", path.display()),
            "fix or delete .ferry/pin-state.json",
        ),
        PinError::LedgerCorrupt { path, line, reason } => OpError::new(
            "held-ledger-corrupt",
            format!("{} near line {line}: {reason}", path.display()),
            "run `ferry pin status` for detail",
        ),
        PinError::Io { source, .. } => OpError::from(source),
        other => OpError::new(
            "internal",
            other.to_string(),
            "check the daemon's stderr log",
        ),
    }
}

pub(super) fn log_err(e: ferry_sync_engine::LogError) -> OpError {
    match e {
        ferry_sync_engine::LogError::Corrupt { path, reason, .. } => OpError::new(
            "conflict-log",
            format!("{}: {reason}", path.display()),
            "fix or archive .ferry/conflicts.jsonl",
        ),
        ferry_sync_engine::LogError::Io { source, .. } => OpError::from(source),
    }
}

pub(crate) fn held_by_peer(st: &UiState) -> Result<Vec<(String, Vec<String>)>, OpError> {
    let summary = pin_manager(st).summary().map_err(pin_err)?;
    Ok(summary.held_by_peer.into_iter().collect())
}

pub(crate) fn conflict_entries(state_dir: &Path) -> Result<Vec<Value>, OpError> {
    ferry_sync_engine::list_conflicts(state_dir)
        .map_err(log_err)?
        .iter()
        .map(|e| {
            serde_json::to_value(e).map_err(|err| {
                OpError::new("conflict-log", err.to_string(), "retry the request")
            })
        })
        .collect()
}

pub(super) fn folder_err(e: ferry_folder::FolderError) -> OpError {
    OpError::new(e.code, e.message, e.hint)
}

pub(super) fn share(
    st: &UiState,
    folder: Option<&Path>,
    i_know: bool,
) -> Result<Value, OpError> {
    let root = folder.unwrap_or(st.tree_dir());
    let opened = open_folder(root, st.identity()).map_err(folder_err)?;
    let rules = load_rules(&opened.root, &opened.settings).map_err(folder_err)?;
    let warnings_raw = ferry_ignore::secrets::scan_for_secrets(&rules, &opened.root);
    let warnings: Vec<Value> = warnings_raw
        .iter()
        .map(|w| {
            json!({
                "path": w.path.join("/"),
                "line": w.line,
                "class": w.class.label(),
                "preview": w.preview,
            })
        })
        .collect();
    if !warnings.is_empty() && !i_know {
        let mut msg = format!("{} secret risk(s) would SYNC to other devices:\n", warnings.len());
        for w in warnings_raw.iter().take(20) {
            let loc = w.line.map(|n| format!(":{n}")).unwrap_or_default();
            let _ = writeln!(
                msg,
                "  SECRET RISK [{}] {}{} — {}",
                w.class.label(),
                w.path.join("/"),
                loc,
                w.preview
            );
        }
        if warnings_raw.len() > 20 {
            let _ = writeln!(msg, "  … and {} more", warnings_raw.len() - 20);
        }
        return Err(OpError::new(
            "secrets-found",
            msg.trim_end().to_string(),
            "review each path: exclude it (`ferry ignore '<pattern>'`) or accept the risk with --i-know",
        )
        .with_detail(json!({ "warnings": warnings })));
    }
    let pending = initiate_begin(&opened, st.identity()).map_err(folder_err)?;
    let warnings_reviewed = !warnings.is_empty();
    let short_code = pending.short_code.clone();
    let completed = initiate_complete(pending, &opened, st.identity(), PAIR_TIMEOUT_SECS)
        .map_err(folder_err)?;
    Ok(json!({
        "command": "share",
        "role": "initiate",
        "status": "completed",
        "folder": opened.root.display().to_string(),
        "folder_id": hex_str(&opened.folder_id),
        "peer_device_id": hex_str(&completed.peer_device_id),
        "short_code": short_code,
        "offer_file": completed.offer_path.display().to_string(),
        "warnings_reviewed": warnings_reviewed,
        "warnings": warnings,
    }))
}

pub(super) fn pair_accept(
    st: &UiState,
    payload_path: &Path,
    dir: Option<&Path>,
) -> Result<Value, OpError> {
    let pending = accept_begin(st.identity(), payload_path, dir).map_err(folder_err)?;
    let expected_short_code = pending.expected_short_code.clone();
    let accepted = accept_complete(pending, st.identity(), PAIR_TIMEOUT_SECS).map_err(folder_err)?;
    Ok(json!({
        "command": "pair",
        "role": "accept",
        "status": "completed",
        "folder": accepted.folder.display().to_string(),
        "folder_id": hex_str(&accepted.folder_id),
        "device_id": st.device_hex(),
        "expected_short_code": expected_short_code,
    }))
}

pub(super) fn pin_start(st: &UiState, paths: Option<Vec<String>>) -> Result<Value, OpError> {
    let mut base_agreements = BTreeMap::new();
    for (dev, rec) in AgreementLedger::new(st.state_dir())
        .list_folder(&st.folder_id())
        .map_err(|e| OpError::new("agreement-state", e.to_string(), "check .ferry permissions"))?
    {
        base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
    }

    let base_peers_recorded = base_agreements.len();
    let mgr = pin_manager(st);
    let pid = std::process::id();
    let record = mgr
        .start_session(
            paths.unwrap_or_default(),
            pid,
            st.device_hex(),
            base_agreements,
        )
        .map_err(pin_err)?;

    Ok(json!({
        "command": "pin",
        "action": "start",
        "folder": st.tree_dir().display().to_string(),
        "device_id": st.device_hex(),
        "pid": pid,
        "paths": record.paths,
        "started_at": ferry_platform::time::fmt_rfc3339(record.started_sec),
        "base_peers_recorded": base_peers_recorded,
    }))
}

pub(super) fn pin_stop(st: &UiState) -> Result<Value, OpError> {
    let mgr = pin_manager(st);
    let summary = mgr.summary().map_err(pin_err)?;
    let was_pinned = summary.holding || summary.state == "active" || summary.state == "stale";
    let _ = mgr.stop_session().map_err(pin_err)?;

    let by_peer: serde_json::Map<String, Value> = summary
        .held_by_peer
        .iter()
        .map(|(peer, paths)| (peer.clone(), json!(paths.len())))
        .collect();

    Ok(json!({
        "command": "pin",
        "action": "stop",
        "folder": st.tree_dir().display().to_string(),
        "was_pinned": was_pinned,
        "held_changes": summary.total_held_paths,
        "held_by_peer": Value::Object(by_peer),
    }))
}

pub(super) fn pin_release(st: &UiState) -> Result<Value, OpError> {
    let mgr = pin_manager(st);
    let summary = mgr.summary().map_err(pin_err)?;

    if summary.total_held_paths > 0 {
        return Err(OpError::new(
            "not-implemented",
            format!("{} held change(s) need reconciliation via the CLI", summary.total_held_paths),
            "run `ferry pin release` in this folder on the command line",
        ));
    }

    let pin_ended = mgr.stop_session().map_err(pin_err)?;
    let conflicts_total = conflict_entries(&st.state_dir())?.len();

    Ok(json!({
        "command": "pin",
        "action": "release",
        "folder": st.tree_dir().display().to_string(),
        "peers": [],
        "quarantined": 0,
        "conflicts_recorded": 0,
        "ops_applied": 0,
        "pin_ended": pin_ended,
        "conflicts_total": conflicts_total,
    }))
}
