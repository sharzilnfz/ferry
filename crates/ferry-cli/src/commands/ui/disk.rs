use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde_json::{json, Value};

use ferry_pin::{HeldLedger, PinError, PinRecord, PinStore, PIN_FORMAT_VERSION};
use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex as hex_str;

use super::error::OpError;

const PAIR_TIMEOUT_SECS: u64 = 120;

fn pin_err(e: PinError) -> OpError {
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
        other => OpError::new("internal", other.to_string(), "check server stderr log"),
    }
}

pub fn read_status_from_disk(folder: &Path) -> Result<Value, OpError> {
    let identity = {
        let home = crate::home::ferry_home().map_err(OpError::from)?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| OpError::new("identity-corrupt", e.to_string(), "restore device.key"))?
    };
    let opened = ferry_folder::folder::open_folder(folder, &identity).map_err(OpError::from)?;
    let scan = crate::scan::one_shot(&opened, *identity.public()).map_err(OpError::from)?;
    let manifest_id = hex_str(&scan.manifest_id);
    let device_id = hex_str(identity.public());
    let folder_id = hex_str(&opened.folder_id);

    let ledger = AgreementLedger::new(opened.state_dir());
    let records = ledger
        .list_folder(&opened.folder_id)
        .map_err(|e| OpError::new("agreement-state", e.to_string(), "check .ferry permissions"))?;

    let mut peers = Vec::new();
    for (dev, rec) in records {
        peers.push(json!({
            "device_id": hex_str(&dev),
            "last_agreed_manifest_id": hex_str(&rec.manifest_id),
            "agreed_at": Some(ferry_sync_engine::timefmt::fmt_rfc3339(rec.agreed_sec)),
            "connectivity": "unknown",
        }));
    }
    peers.sort_by(|a, b| a["device_id"].as_str().cmp(&b["device_id"].as_str()));

    let pin_store = PinStore::new(opened.state_dir());
    let pin = match pin_store.load().map_err(pin_err)? {
        None => json!({"state": "none", "holding": false, "paths": []}),
        Some(rec) => {
            let state = if rec.released {
                "released"
            } else if rec.holding() {
                "active"
            } else {
                "stale"
            };
            json!({
                "state": state,
                "holding": state == "active",
                "paths": rec.paths,
            })
        }
    };

    let held_ledger = HeldLedger::new(opened.state_dir());
    let mut held_by_peer = serde_json::Map::new();
    let mut held_total = 0usize;
    for peer in held_ledger.peers().map_err(pin_err)? {
        let entries = held_ledger.load_peer(&peer).map_err(pin_err)?;
        let paths = ferry_pin::distinct_paths(&entries);
        held_total += paths.len();
        held_by_peer.insert(peer, json!(paths));
    }

    let conflicts = ferry_sync_engine::list_conflicts(&opened.state_dir()).map_err(|e| {
        OpError::new(
            "conflict-log",
            e.to_string(),
            "fix or archive .ferry/conflicts.jsonl",
        )
    })?;

    Ok(json!({
        "command": "status",
        "folder": opened.root.display().to_string(),
        "folder_id": folder_id,
        "device_id": device_id,
        "manifest_id": manifest_id,
        "scanned": {
            "files": scan.stats.files,
            "dirs": scan.stats.dirs,
            "symlinks": scan.stats.symlinks,
            "bytes_chunked": scan.stats.bytes_chunked,
        },
        "pending_changes": Value::Null,
        "pin": pin,
        "held_changes": held_total,
        "held_by_peer": Value::Object(held_by_peer),
        "peers": peers,
        "conflicts": conflicts.len(),
    }))
}

pub fn read_conflicts_from_disk(folder: &Path) -> Result<Value, OpError> {
    let state_dir = folder.join(".ferry");
    let entries = if state_dir.exists() {
        ferry_sync_engine::list_conflicts(&state_dir)
            .map_err(|e| {
                OpError::new(
                    "conflict-log",
                    e.to_string(),
                    "fix or archive .ferry/conflicts.jsonl",
                )
            })?
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    Ok(json!({
        "command": "conflicts",
        "folder": folder.display().to_string(),
        "entries": entries,
    }))
}

pub fn share_folder(target: &Path, i_know: bool) -> Result<Value, OpError> {
    let identity = {
        let home = crate::home::ferry_home().map_err(OpError::from)?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| OpError::new("identity-corrupt", e.to_string(), "restore device.key"))?
    };
    let opened = ferry_folder::folder::open_folder(target, &identity).map_err(OpError::from)?;
    let rules = ferry_folder::folder::load_rules(&opened.root, &opened.settings)
        .map_err(OpError::from)?;
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
        let mut msg = format!(
            "{} secret risk(s) would SYNC to other devices:\n",
            warnings.len()
        );
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

    let pending = ferry_folder::pairing::initiate_begin(&opened, &identity).map_err(OpError::from)?;
    let warnings_reviewed = !warnings.is_empty();
    let short_code = pending.short_code.clone();
    let completed = ferry_folder::pairing::initiate_complete(
        pending,
        &opened,
        &identity,
        PAIR_TIMEOUT_SECS,
    )
    .map_err(OpError::from)?;

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

pub fn pair_accept_folder(payload_path: &Path, dir: Option<&Path>) -> Result<Value, OpError> {
    let identity = {
        let home = crate::home::ferry_home().map_err(OpError::from)?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| OpError::new("identity-corrupt", e.to_string(), "restore device.key"))?
    };
    let pending = ferry_folder::pairing::accept_begin(&identity, payload_path, dir)
        .map_err(OpError::from)?;
    let expected_short_code = pending.expected_short_code.clone();
    let accepted = ferry_folder::pairing::accept_complete(pending, &identity, PAIR_TIMEOUT_SECS)
        .map_err(OpError::from)?;

    Ok(json!({
        "command": "pair",
        "role": "accept",
        "status": "completed",
        "folder": accepted.folder.display().to_string(),
        "folder_id": hex_str(&accepted.folder_id),
        "device_id": hex_str(identity.public()),
        "expected_short_code": expected_short_code,
    }))
}

pub fn pin_start_disk(folder: &Path, paths: Option<Vec<String>>) -> Result<Value, OpError> {
    let identity = {
        let home = crate::home::ferry_home().map_err(OpError::from)?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| OpError::new("identity-corrupt", e.to_string(), "restore device.key"))?
    };
    let opened = ferry_folder::folder::open_folder(folder, &identity).map_err(OpError::from)?;
    let scope: Vec<String> = match paths {
        Some(p) if !p.is_empty() => p,
        _ => vec!["*".to_string()],
    };

    let mut base_agreements = BTreeMap::new();
    for (dev, rec) in AgreementLedger::new(opened.state_dir())
        .list_folder(&opened.folder_id)
        .map_err(|e| OpError::new("agreement-state", e.to_string(), "check .ferry permissions"))?
    {
        base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
    }

    let (sec, nsec) = ferry_sync_engine::timefmt::now_unix();
    let pid = std::process::id();
    let base_peers_recorded = base_agreements.len();
    let record = PinRecord {
        format_version: PIN_FORMAT_VERSION,
        device_id: hex_str(identity.public()),
        pid,
        started_sec: sec,
        started_nsec: nsec,
        paths: scope.clone(),
        released: false,
        base_agreements,
        proc_start_token: None,
    };
    PinStore::new(opened.state_dir()).start(&record).map_err(pin_err)?;

    Ok(json!({
        "command": "pin",
        "action": "start",
        "folder": opened.root.display().to_string(),
        "device_id": hex_str(identity.public()),
        "pid": pid,
        "paths": scope,
        "started_at": ferry_sync_engine::timefmt::fmt_rfc3339(sec),
        "base_peers_recorded": base_peers_recorded,
    }))
}

pub fn pin_stop_disk(folder: &Path) -> Result<Value, OpError> {
    let identity = {
        let home = crate::home::ferry_home().map_err(OpError::from)?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| OpError::new("identity-corrupt", e.to_string(), "restore device.key"))?
    };
    let opened = ferry_folder::folder::open_folder(folder, &identity).map_err(OpError::from)?;
    let store = PinStore::new(opened.state_dir());
    let was_pinned = store.load().map_err(pin_err)?.is_some();
    let was_pinned = store.mark_released().map_err(pin_err)? && was_pinned;

    let held_ledger = HeldLedger::new(opened.state_dir());
    let mut by_peer = serde_json::Map::new();
    let mut total = 0usize;
    for peer in held_ledger.peers().map_err(pin_err)? {
        let entries = held_ledger.load_peer(&peer).map_err(pin_err)?;
        let paths = ferry_pin::distinct_paths(&entries);
        total += paths.len();
        by_peer.insert(peer, json!(paths.len()));
    }

    Ok(json!({
        "command": "pin",
        "action": "stop",
        "folder": opened.root.display().to_string(),
        "was_pinned": was_pinned,
        "held_changes": total,
        "held_by_peer": Value::Object(by_peer),
    }))
}

pub fn pin_release_disk(folder: &Path) -> Result<Value, OpError> {
    let identity = {
        let home = crate::home::ferry_home().map_err(OpError::from)?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| OpError::new("identity-corrupt", e.to_string(), "restore device.key"))?
    };
    let opened = ferry_folder::folder::open_folder(folder, &identity).map_err(OpError::from)?;

    let held_ledger = HeldLedger::new(opened.state_dir());
    let mut total_held = 0usize;
    for peer in held_ledger.peers().map_err(pin_err)? {
        let entries = held_ledger.load_peer(&peer).map_err(pin_err)?;
        total_held += ferry_pin::distinct_paths(&entries).len();
    }

    if total_held > 0 {
        return Err(OpError::new(
            "not-implemented",
            format!("{total_held} held change(s) need reconciliation via the CLI"),
            "run `ferry pin release` in this folder on the command line",
        ));
    }

    let pin_ended = PinStore::new(opened.state_dir()).mark_released().map_err(pin_err)?;
    let conflicts_total = ferry_sync_engine::list_conflicts(&opened.state_dir())
        .map_err(|e| OpError::new("conflict-log", e.to_string(), "fix .ferry/conflicts.jsonl"))?
        .len();

    Ok(json!({
        "command": "pin",
        "action": "release",
        "folder": opened.root.display().to_string(),
        "peers": [],
        "quarantined": 0,
        "conflicts_recorded": 0,
        "ops_applied": 0,
        "pin_ended": pin_ended,
        "conflicts_total": conflicts_total,
    }))
}
