







use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use ferry_pin::PinManager;
use ferry_platform::time::fmt_rfc3339;
use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex;
use ferry_sync_engine::list_conflicts;
use serde_json::json;

use crate::error::CliResult;
use crate::folder::{self, OpenFolder};
use crate::out::Output;


struct PeerRow {
    device_id: String,
    last_agreed_manifest_id: Option<String>,
    agreed_at: Option<String>,
    connectivity: String,
}

pub fn run(folder: &Path) -> CliResult<Output> {
    if let Some(snap) = crate::ipc::query_status(folder) {
        return Ok(output_from_snapshot(&snap));
    }

    run_offline(folder)
}

fn output_from_snapshot(snap: &ferry_ipc::EngineSnapshot) -> Output {
    let manifest_id = snap.manifest_id.clone().unwrap_or_default();
    let json_doc = json!({
        "command": "status",
        "folder": snap.folder,
        "folder_id": snap.folder_id,
        "device_id": snap.device_id,
        "manifest_id": manifest_id,
        "scanned": {
            "files": snap.scanned.files,
            "dirs": snap.scanned.dirs,
            "symlinks": snap.scanned.symlinks,
            "bytes_chunked": snap.scanned.bytes_chunked,
        },
        "pending_changes": snap.pending_changes,
        "pin": {
            "state": snap.pin.state,
            "holding": snap.pin.holding,
            "paths": snap.pin.paths,
        },
        "held_changes": snap.held_changes,
        "held_by_peer": snap.held_by_peer,
        "peers": snap.peers.iter().map(|p| json!({
            "device_id": p.device_id,
            "last_agreed_manifest_id": p.last_agreed_manifest_id,
            "agreed_at": p.agreed_at,
            "connectivity": p.connectivity,
        })).collect::<Vec<_>>(),
        "conflicts": snap.conflicts,
    });

    let peer_rows: Vec<PeerRow> = snap
        .peers
        .iter()
        .map(|p| PeerRow {
            device_id: p.device_id.clone(),
            last_agreed_manifest_id: p.last_agreed_manifest_id.clone(),
            agreed_at: p.agreed_at.clone(),
            connectivity: p.connectivity.clone(),
        })
        .collect();

    let human = render_human(
        &snap.folder,
        &snap.folder_id,
        &snap.device_id,
        snap.scanned.files,
        snap.scanned.dirs,
        snap.scanned.symlinks,
        &manifest_id,
        snap.pending_changes,
        snap.conflicts,
        &snap.pin.state,
        &snap.pin.paths,
        snap.held_changes,
        &peer_rows,
    );

    Output::new(json_doc, human)
}

fn run_offline(folder: &Path) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let identity = {
        let home = crate::home::ferry_home()?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)).map_err(|e| {
            crate::error::CliError::new(
                "identity-corrupt",
                e.to_string(),
                "restore or replace your device.key",
            )
        })
    }?;
    let device_id = hex(identity.public());

    
    let scan = crate::scan::one_shot(&opened, *identity.public())?;
    let manifest = &scan.manifest;
    let manifest_id = hex(&scan.manifest_id);

    let peers = list_peers(&opened)?;
    let conflicts = list_conflicts(&opened.state_dir()).map_err(|e| {
        crate::error::CliError::new(
            "conflict-log",
            e.to_string(),
            "fix or archive .ferry/conflicts.jsonl",
        )
    })?;

    
    
    let pending: Option<i64> = match most_recent_base(&opened)? {
        BaseLookup::NoAgreement => None,
        BaseLookup::Unreadable => Some(-1),
        BaseLookup::Base(base_manifest) => Some(
            ferry_store::diff::diff_manifests(&opened.store, &base_manifest, manifest).map_or(
                -1,
                |cs| {
                    (cs.added.len()
                        + cs.removed.len()
                        + cs.content_modified.len()
                        + cs.type_changed.len()
                        + cs.metadata_modified.len()) as i64
                },
            ),
        ),
    };

    
    
    let pin_summary = PinManager::new(opened.state_dir()).summary().map_err(|e| {
        crate::error::CliError::new(
            "pin-state-corrupt",
            e.to_string(),
            "inspect .ferry/pin-state.json",
        )
    })?;
    let pin_state = pin_summary.state;
    let pin_paths = pin_summary.paths;
    let pin_holding = pin_summary.holding;
    let held_total = pin_summary.total_held_paths;
    let mut held_by_peer = serde_json::Map::new();
    for (peer, paths) in pin_summary.held_by_peer {
        held_by_peer.insert(peer, json!(paths));
    }

    let folder_str = opened.root.display().to_string();
    let folder_id_str = hex(&opened.folder_id);

    let json_doc = json!({
        "command": "status",
        "folder": folder_str,
        "folder_id": folder_id_str,
        "device_id": device_id,
        "manifest_id": manifest_id,
        "scanned": {
            "files": scan.stats.files,
            "dirs": scan.stats.dirs,
            "symlinks": scan.stats.symlinks,
            "bytes_chunked": scan.stats.bytes_chunked,
        },
        "pending_changes": pending,
        "pin": {
            "state": pin_state,
            "holding": pin_holding,
            "paths": pin_paths,
        },
        "held_changes": held_total,
        "held_by_peer": held_by_peer,
        "peers": peers.iter().map(|p| json!({
            "device_id": p.device_id,
            "last_agreed_manifest_id": p.last_agreed_manifest_id,
            "agreed_at": p.agreed_at,
            "connectivity": p.connectivity,
        })).collect::<Vec<_>>(),
        "conflicts": conflicts.len(),
    });

    let human = render_human(
        &folder_str,
        &folder_id_str,
        &device_id,
        scan.stats.files as u64,
        scan.stats.dirs as u64,
        scan.stats.symlinks as u64,
        &manifest_id,
        pending,
        conflicts.len(),
        &pin_state,
        &pin_paths,
        held_total,
        &peers,
    );

    Ok(Output::new(json_doc, human))
}

#[allow(clippy::too_many_arguments)]
fn render_human(
    folder: &str,
    folder_id: &str,
    device_id: &str,
    scanned_files: u64,
    scanned_dirs: u64,
    scanned_symlinks: u64,
    manifest_id: &str,
    pending: Option<i64>,
    conflicts: usize,
    pin_state: &str,
    pin_paths: &[String],
    held_total: usize,
    peers: &[PeerRow],
) -> String {
    let mut human = String::new();
    let _ = writeln!(human, "Folder     {folder} ({folder_id})");
    let _ = writeln!(human, "Device     {device_id}");
    let _ = writeln!(
        human,
        "Scan       {scanned_files} files, {scanned_dirs} dirs, {scanned_symlinks} symlinks"
    );
    let _ = writeln!(human, "Manifest   {manifest_id}");
    match pending {
        Some(n) if n >= 0 => {
            let _ = writeln!(human, "Pending    {n} change(s) vs last agreement");
        }
        Some(_) => human.push_str("Pending    unknown (base manifest unreadable)\n"),
        None => human.push_str("Pending    no agreement yet\n"),
    }
    let _ = writeln!(human, "Conflicts  {conflicts}");
    match pin_state {
        "none" => human.push_str("Pin        none\n"),
        s => {
            let _ = writeln!(human, "Pin        {} ({})", s, pin_paths.join(", "));
        }
    }
    if held_total == 0 {
        human.push_str("Held       nothing\n");
    } else {
        let _ = writeln!(
            human,
            "Held       {held_total} path(s) — `ferry pin release` reconciles them"
        );
    }
    if peers.is_empty() {
        human.push_str("Peers      none yet — run `ferry pair`\n");
    } else {
        human.push_str("Peers:\n");
        for p in peers {
            let agreed = p
                .last_agreed_manifest_id
                .clone()
                .unwrap_or_else(|| "-".into());
            let at = p.agreed_at.clone().unwrap_or_else(|| "-".into());
            let _ = writeln!(
                human,
                "  {}  agreed={} at={} link={}",
                p.device_id,
                &agreed[..24.min(agreed.len())],
                at,
                p.connectivity
            );
        }
    }
    human
}


pub fn scan_now(opened: &OpenFolder) -> CliResult<crate::scan::OneShot> {
    let identity = {
        let home = crate::home::ferry_home()?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)).map_err(|e| {
            crate::error::CliError::new(
                "identity-corrupt",
                e.to_string(),
                "restore or replace your device.key",
            )
        })
    }?;
    crate::scan::one_shot(opened, *identity.public())
}



fn list_peers(opened: &OpenFolder) -> CliResult<Vec<PeerRow>> {
    let ledger = AgreementLedger::new(opened.state_dir());
    let mut rows = Vec::new();
    for (dev, rec) in ledger.list_folder(&opened.folder_id).map_err(|e| {
        crate::error::CliError::new(
            "agreement-state",
            e.to_string(),
            "check .ferry/agreement permissions",
        )
    })? {
        let dev_hex = hex(&dev);
        let connectivity = probe_peer(opened, &dev_hex);
        rows.push(PeerRow {
            device_id: dev_hex,
            last_agreed_manifest_id: Some(hex(&rec.manifest_id)),
            agreed_at: Some(format_agreed_time(&rec)),
            connectivity: connectivity.to_string(),
        });
    }
    Ok(rows)
}

fn most_recent_base(opened: &OpenFolder) -> CliResult<BaseLookup> {
    let ledger = AgreementLedger::new(opened.state_dir());
    let records = ledger.list_folder(&opened.folder_id).map_err(|e| {
        crate::error::CliError::new(
            "agreement-state",
            e.to_string(),
            "check .ferry/agreement permissions",
        )
    })?;
    
    
    let best = records
        .into_iter()
        .max_by_key(|(_, rec)| (rec.agreed_sec, rec.agreed_nsec));
    let Some((_, rec)) = best else {
        return Ok(BaseLookup::NoAgreement);
    };
    match opened
        .store
        .get(ferry_store::format::BlobKind::Manifest, &rec.manifest_id)
    {
        Ok(bytes) => match ferry_store::manifest::parse_manifest(&bytes) {
            Ok(m) => Ok(BaseLookup::Base(m)),
            Err(e) => Err(crate::error::CliError::new(
                "store",
                e.to_string(),
                "the agreed manifest blob is damaged",
            )),
        },
        
        
        Err(_) => Ok(BaseLookup::Unreadable),
    }
}


enum BaseLookup {
    
    NoAgreement,
    
    Unreadable,
    
    Base(ferry_store::manifest::RootManifest),
}



fn probe_peer(opened: &OpenFolder, peer_hex: &str) -> &'static str {
    let addr_path = opened
        .state_dir()
        .join("peers")
        .join(format!("{peer_hex}.addr"));
    let Ok(text) = std::fs::read_to_string(addr_path) else {
        return "unknown";
    };
    let Ok(addr) = text.trim().parse() else {
        return "unknown";
    };
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        Ok(_) => "reachable",
        Err(_) => "unreachable",
    }
}


pub fn format_agreed_time(rec: &ferry_store::agreement::AgreedRecord) -> String {
    fmt_rfc3339(rec.agreed_sec)
}
