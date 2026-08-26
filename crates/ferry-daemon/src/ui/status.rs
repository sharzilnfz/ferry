//! Read-only dashboard documents: cached engine state + cheap `.ferry/`
//! metadata. Nothing here rescans or hashes the tree.

use serde_json::{json, Value};

use ferry_store::agreement::{AgreedRecord, AgreementLedger};
use ferry_store::format::hex as hex_str;

use super::{OpError, UiState};

/// `GET /api/status` — `ferry status --json` shape from cached state.
pub(super) fn status_doc(st: &UiState) -> Result<Value, OpError> {
    let Some(manifest_id) = st.handle().current_manifest_id() else {
        return Err(OpError::new(
            "warming-up",
            "the engine has not completed its first poll tick",
            "retry shortly",
        ));
    };

    let counts = st.handle().scan_counts().unwrap_or_default();
    let records = AgreementLedger::new(st.state_dir())
        .list_folder(&st.folder_id())
        .map_err(|e| OpError::new("agreement-state", e.to_string(), "check .ferry permissions"))?;
    let peers = peer_rows(st, &records)?;
    let pin = pin_view(st)?;
    let held = super::actions::held_by_peer(st)?;
    let held_total: usize = held.iter().map(|(_, paths)| paths.len()).sum();
    let conflicts_total = conflict_entries(st)?.len();

    let pending = match st.handle().pending_changes() {
        Some(n) => json!(n),
        None => Value::Null,
    };

    Ok(json!({
        "command": "status",
        "folder": st.tree_dir().display().to_string(),
        "folder_id": hex_str(&st.folder_id()),
        "device_id": st.device_hex(),
        "manifest_id": hex_str(&manifest_id),
        "scanned": {
            "files": counts.files,
            "dirs": counts.dirs,
            "symlinks": counts.symlinks,
            "bytes_chunked": counts.bytes_chunked,
        },
        "pending_changes": pending,
        "pin": pin,
        "held_changes": held_total,
        "held_by_peer": held_map(&held),
        "peers": peers,
        "conflicts": conflicts_total,
    }))
}

/// `GET /api/conflicts` — `.ferry/conflicts.jsonl` lines verbatim.
pub(super) fn conflicts_doc(st: &UiState) -> Result<Value, OpError> {
    let entries = conflict_entries(st)?;
    Ok(json!({
        "command": "conflicts",
        "folder": st.tree_dir().display().to_string(),
        "entries": entries,
    }))
}

fn peer_rows(st: &UiState, records: &[([u8; 32], AgreedRecord)]) -> Result<Value, OpError> {
    let mut rows: Vec<([u8; 32], String, &AgreedRecord)> = records
        .iter()
        .map(|(dev, rec)| (*dev, hex_str(dev), rec))
        .collect();
    rows.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(Value::Array(
        rows.into_iter()
            .map(|(dev_bytes, dev_hex, rec)| {
                json!({
                    "device_id": dev_hex,
                    "last_agreed_manifest_id": hex_str(&rec.manifest_id),
                    "agreed_at": Some(ferry_platform::time::fmt_rfc3339(rec.agreed_sec)),
                    "connectivity": st.handle().peer_connectivity(&dev_bytes),
                })
            })
            .collect(),
    ))
}

/// The pin block of the status document, read via ferry-pin's
/// [`ferry_pin::PinManager`] so liveness uses the same platform
/// proc-start-token evidence as the CLI (`ferry pin status` and this view
/// can no longer disagree about staleness).
fn pin_view(st: &UiState) -> Result<Value, OpError> {
    let summary = super::actions::pin_manager(st)
        .summary()
        .map_err(super::actions::pin_err)?;
    Ok(json!({
        "state": summary.state,
        "holding": summary.holding,
        "paths": summary.paths,
    }))
}

fn held_map(held: &[(String, Vec<String>)]) -> Value {
    let mut map = serde_json::Map::new();
    for (peer, paths) in held {
        map.insert(peer.clone(), json!(paths));
    }
    Value::Object(map)
}

pub(crate) fn conflict_entries(st: &UiState) -> Result<Vec<Value>, OpError> {
    super::actions::conflict_entries(&st.state_dir())
}
