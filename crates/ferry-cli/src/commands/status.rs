//! `ferry status`: one folder's state of the world.
//!
//! Honest v0 notes baked into the output:
//! - The reported manifest comes from a FRESH full scan (`ferry status`
  //! re-snapshots so ids are current; big trees cost a scan).
//! - Connectivity is best-effort TCP reachability of the last known peer
//!   address; without a recorded address it is "unknown".

use std::path::{Path, PathBuf};
use std::time::Duration;

use ferry_store::format::hex;
use ferry_sync_engine::{list_conflicts, PeerState};
use serde_json::json;

use crate::error::CliResult;
use crate::folder::{self, OpenFolder};
use crate::out::Output;

/// One peer line as it appears in both renderings.
struct PeerRow {
    device_id: String,
    last_agreed_manifest_id: Option<String>,
    agreed_at: Option<String>,
    connectivity: &'static str,
}

pub fn run(folder: &Path) -> CliResult<Output> {
    let opened = folder::open_folder(folder)?;
    let identity = {
        let home = crate::home::ferry_home()?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| crate::error::CliError::new("identity-corrupt", e.to_string(), "restore or replace your device.key"))
    }?;
    let device_id = hex(identity.public());

    // Fresh policy-aware scan: manifest id reflects the tree RIGHT NOW.
    let scan = crate::scan::one_shot(&opened, *identity.public())?;
    let manifest = &scan.manifest;
    let manifest_id = hex(&scan.manifest_id);

    let peers = list_peers(&opened)?;
    let conflicts = list_conflicts(&opened.state_dir())
        .map_err(|e| crate::error::CliError::new("conflict-log", e.to_string(), "fix or archive .ferry/conflicts.jsonl"))?;

    // Pending changes: diff against the most recent agreement (any peer).
    // Negative means "unknown"; JSON null means no agreement exists yet.
    let pending: Option<i64> = match most_recent_base(&opened)? {
        BaseLookup::NoAgreement => None,
        BaseLookup::Unreadable => Some(-1),
        BaseLookup::Base(base_manifest) => {
            Some(ferry_store::diff::diff_manifests(&opened.store, &base_manifest, manifest)
                .map(|cs| {
                    (cs.added.len()
                        + cs.removed.len()
                        + cs.content_modified.len()
                        + cs.type_changed.len()
                        + cs.metadata_modified.len()) as i64
                })
                .unwrap_or(-1))
        }
    };

    let json_doc = json!({
        "command": "status",
        "folder": opened.root.display().to_string(),
        "folder_id": hex(&opened.folder_id),
        "device_id": device_id,
        "manifest_id": manifest_id,
        "scanned": {
            "files": scan.stats.files,
            "dirs": scan.stats.dirs,
            "symlinks": scan.stats.symlinks,
            "bytes_chunked": scan.stats.bytes_chunked,
        },
        "pending_changes": pending,
        "peers": peers.iter().map(|p| json!({
            "device_id": p.device_id,
            "last_agreed_manifest_id": p.last_agreed_manifest_id,
            "agreed_at": p.agreed_at,
            "connectivity": p.connectivity,
        })).collect::<Vec<_>>(),
        "conflicts": conflicts.len(),
    });

    let mut human = String::new();
    human.push_str(&format!("Folder     {} ({})\n", display(opened.root.display()), hex(&opened.folder_id)));
    human.push_str(&format!("Device     {}\n", device_id));
    human.push_str(&format!(
        "Scan       {} files, {} dirs, {} symlinks\n",
        scan.stats.files, scan.stats.dirs, scan.stats.symlinks
    ));
    human.push_str(&format!("Manifest   {manifest_id}\n"));
    match pending {
        Some(n) if n >= 0 => human.push_str(&format!("Pending    {n} change(s) vs last agreement\n")),
        Some(_) => human.push_str("Pending    unknown (base manifest unreadable)\n"),
        None => human.push_str("Pending    no agreement yet\n"),
    }
    human.push_str(&format!("Conflicts  {}\n", conflicts.len()));
    if peers.is_empty() {
        human.push_str("Peers      none yet — run `ferry pair`\n");
    } else {
        human.push_str("Peers:\n");
        for p in &peers {
            let agreed = p.last_agreed_manifest_id.clone().unwrap_or_else(|| "-".into());
            let at = p.agreed_at.clone().unwrap_or_else(|| "-".into());
            human.push_str(&format!(
                "  {}  agreed={} at={} link={}\n",
                p.device_id, &agreed[..24.min(agreed.len())], at, p.connectivity
            ));
        }
    }

    Ok(Output::new(json_doc, human))
}

fn display(d: std::path::Display<'_>) -> String {
    d.to_string()
}

/// A fresh policy-aware scan into the folder's store.
pub fn scan_now(
    opened: &OpenFolder,
) -> CliResult<crate::scan::OneShot> {
    let identity = {
        let home = crate::home::ferry_home()?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
            .map_err(|e| crate::error::CliError::new("identity-corrupt", e.to_string(), "restore or replace your device.key"))
    }?;
    crate::scan::one_shot(opened, *identity.public())
}

/// Every peer this folder has agreement state for, plus best-effort
/// connectivity.
fn list_peers(opened: &OpenFolder) -> CliResult<Vec<PeerRow>> {
    let ps = PeerState::new(opened.state_dir());
    let mut rows = Vec::new();
    let dir = opened.state_dir().join("peers");
    let mut names: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    for path in names {
        let Some(dev) = ferry_sync_engine::agree::peer_from_path(&path) else { continue };
        let rec = ps.load(&dev).ok().flatten();
        let dev_hex = hex(&dev);
        let connectivity = probe_peer(opened, &dev_hex);
        rows.push(PeerRow {
            device_id: dev_hex,
            last_agreed_manifest_id: rec.as_ref().map(|r| hex(&r.manifest_id)),
            agreed_at: rec.as_ref().map(format_agreed_time),
            connectivity,
        });
    }
    Ok(rows)
}

fn most_recent_base(opened: &OpenFolder) -> CliResult<BaseLookup> {
    let ps = PeerState::new(opened.state_dir());
    let dir = opened.state_dir().join("peers");
    let mut best: Option<(i64, u32)> = None;
    let mut best_id: Option<[u8; 32]> = None;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for path in rd.flatten().map(|e| e.path()) {
            let Some(dev) = ferry_sync_engine::agree::peer_from_path(&path) else { continue };
            if let Ok(Some(rec)) = ps.load(&dev) {
                if best.map_or(true, |(s, n)| (rec.agreed_sec, rec.agreed_nsec) > (s, n)) {
                    best = Some((rec.agreed_sec, rec.agreed_nsec));
                    best_id = Some(rec.manifest_id);
                }
            }
        }
    }
    let Some(mid) = best_id else { return Ok(BaseLookup::NoAgreement) };
    match opened.store.get(ferry_store::format::BlobKind::Manifest, &mid) {
        Ok(bytes) => match ferry_store::manifest::parse_manifest(&bytes) {
            Ok(m) => Ok(BaseLookup::Base(m)),
            Err(e) => Err(crate::error::CliError::new(
                "store",
                e.to_string(),
                "the agreed manifest blob is damaged",
            )),
        },
        // Record exists but its manifest object is gone: "unknown", never a
        // fake "no agreement".
        Err(_) => Ok(BaseLookup::Unreadable),
    }
}

/// Outcome of looking up the most recent agreement's base manifest.
enum BaseLookup {
    /// No peer agreement recorded yet.
    NoAgreement,
    /// Agreement exists but its manifest object is unreadable.
    Unreadable,
    /// The agreed manifest to diff against.
    Base(ferry_store::manifest::RootManifest),
}

/// Best-effort TCP reachability against the address a previous daemon/sync
/// run recorded for this peer. No address on file => "unknown".
fn probe_peer(opened: &OpenFolder, peer_hex: &str) -> &'static str {
    let addr_path = opened.state_dir().join("peers").join(format!("{peer_hex}.addr"));
    let Ok(text) = std::fs::read_to_string(addr_path) else {
        return "unknown";
    };
    let Ok(addr) = text.trim().parse() else { return "unknown" };
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        Ok(_) => "reachable",
        Err(_) => "unreachable",
    }
}

/// Fixed UTC rendering of an agreement timestamp (no local timezone drift).
pub fn format_agreed_time(rec: &ferry_sync_engine::AgreedRecord) -> String {
    ferry_sync_engine::timefmt::fmt_rfc3339(rec.agreed_sec)
}
