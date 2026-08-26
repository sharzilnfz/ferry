//! Shared daemon state and engine snapshot generation.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use ferry_crypto::identity::DeviceIdentity;
use ferry_ipc::{
    DaemonMessage, EngineSnapshot, PeerStatusView, PinView, ScanStatsView,
};
use ferry_pin::{HeldLedger, PinError, PinRecord, PinStore, PIN_FORMAT_VERSION};
use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex as hex_str;
use ferry_sync::EngineHandle;

/// Shared runtime state of the daemon, accessible by IPC handlers and background watchers.
pub struct DaemonState {
    handle: EngineHandle,
    store_dir: PathBuf,
    tree_dir: PathBuf,
    folder_id: [u8; 16],
    device_hex: String,
    identity: DeviceIdentity,
    broadcast_tx: tokio::sync::broadcast::Sender<DaemonMessage>,
}

impl DaemonState {
    /// Create a new `DaemonState`.
    pub fn new(
        handle: EngineHandle,
        store_dir: PathBuf,
        tree_dir: PathBuf,
        folder_id: [u8; 16],
        identity: DeviceIdentity,
        broadcast_tx: tokio::sync::broadcast::Sender<DaemonMessage>,
    ) -> Self {
        let device_hex = hex_str(identity.public());
        Self {
            handle,
            store_dir,
            tree_dir,
            folder_id,
            device_hex,
            identity,
            broadcast_tx,
        }
    }

    pub fn handle(&self) -> &EngineHandle {
        &self.handle
    }

    pub fn store_dir(&self) -> &Path {
        &self.store_dir
    }

    pub fn tree_dir(&self) -> &Path {
        &self.tree_dir
    }

    pub fn state_dir(&self) -> PathBuf {
        self.store_dir.join(".ferry")
    }

    pub fn folder_id(&self) -> [u8; 16] {
        self.folder_id
    }

    pub fn device_hex(&self) -> &str {
        &self.device_hex
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    pub fn broadcast_tx(&self) -> &tokio::sync::broadcast::Sender<DaemonMessage> {
        &self.broadcast_tx
    }

    /// Broadcast a daemon message to all connected IPC clients.
    pub fn broadcast(&self, msg: DaemonMessage) {
        // Send ignores SendError when there are no active subscribers.
        let _ = self.broadcast_tx.send(msg);
    }

    /// Generate an `EngineSnapshot` capturing the current state of the folder and engine.
    pub fn snapshot(&self) -> EngineSnapshot {
        let root = self.handle.root_id();
        let manifest_id_hex = root.map(|r| hex_str(&r));
        let state = if root.is_some() {
            "idle".to_string()
        } else {
            "initializing".to_string()
        };

        let scanned = self
            .handle
            .scan_counts()
            .map(|s| {
                ScanStatsView::new(
                    s.files as u64,
                    s.dirs as u64,
                    s.symlinks as u64,
                    s.bytes_chunked,
                )
            })
            .unwrap_or_default();

        let pending_changes = self.handle.pending_changes();

        let pin_view = match PinStore::new(self.state_dir()).load() {
            Ok(Some(rec)) => {
                let st = if rec.released {
                    "released"
                } else if rec.liveness() == ferry_pin::Liveness::Alive {
                    "active"
                } else {
                    "stale"
                };
                PinView {
                    state: st.to_string(),
                    holding: st == "active",
                    paths: rec.paths,
                }
            }
            _ => PinView::none(),
        };

        let mut held_by_peer = HashMap::new();
        let mut held_total = 0;
        if let Ok(peers) = HeldLedger::new(self.state_dir()).peers() {
            let ledger = HeldLedger::new(self.state_dir());
            for p in peers {
                if let Ok(entries) = ledger.load_peer(&p) {
                    let paths = ferry_pin::distinct_paths(&entries);
                    held_total += paths.len();
                    held_by_peer.insert(p, paths);
                }
            }
        }

        let peers = match AgreementLedger::new(self.state_dir()).list_folder(&self.folder_id) {
            Ok(records) => {
                let mut list = Vec::new();
                for (dev, rec) in records {
                    list.push(PeerStatusView {
                        device_id: hex_str(&dev),
                        last_agreed_manifest_id: Some(hex_str(&rec.manifest_id)),
                        agreed_at: Some(crate::timefmt::fmt_rfc3339(rec.agreed_sec)),
                        connectivity: self.handle.peer_connectivity(&dev).to_string(),
                    });
                }
                list.sort_by(|a, b| a.device_id.cmp(&b.device_id));
                list
            }
            Err(_) => Vec::new(),
        };

        let conflicts = ferry_sync_engine::list_conflicts(&self.state_dir())
            .map_or(0, |c| c.len());

        EngineSnapshot {
            folder: self.tree_dir.display().to_string(),
            folder_id: hex_str(&self.folder_id),
            device_id: self.device_hex.clone(),
            manifest_id: manifest_id_hex,
            state,
            scanned,
            pending_changes,
            pin: pin_view,
            held_changes: held_total,
            held_by_peer,
            peers,
            conflicts,
        }
    }

    /// Start a session pin on the specified paths.
    pub fn start_pin(&self, paths: Vec<String>) -> Result<PinRecord, PinError> {
        let scope = if paths.is_empty() {
            vec!["*".to_string()]
        } else {
            paths
        };

        let mut base_agreements = BTreeMap::new();
        if let Ok(records) = AgreementLedger::new(self.state_dir()).list_folder(&self.folder_id) {
            for (dev, rec) in records {
                base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
            }
        }

        let (sec, nsec) = crate::timefmt::now_unix();
        let pid = std::process::id();
        let record = PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: self.device_hex.clone(),
            pid,
            started_sec: sec,
            started_nsec: nsec,
            paths: scope,
            released: false,
            base_agreements,
            proc_start_token: None,
        };

        PinStore::new(self.state_dir()).start(&record)?;
        Ok(record)
    }

    /// Release an active session pin.
    pub fn release_pin(&self) -> Result<bool, PinError> {
        PinStore::new(self.state_dir()).mark_released()
    }

    /// Trigger an audit-grade filesystem rescan.
    pub fn trigger_scan(&self) {
        self.handle.trigger_scan();
    }

    /// List recorded conflicts.
    pub fn list_conflicts(&self) -> Result<Vec<ferry_sync_engine::ConflictEntry>, ferry_sync_engine::LogError> {
        ferry_sync_engine::list_conflicts(&self.state_dir())
    }
}
