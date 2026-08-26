//! Shared daemon state and engine snapshot generation.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use ferry_crypto::identity::DeviceIdentity;
use ferry_ipc::{DaemonMessage, EngineSnapshot, PeerStatusView, PinView, ScanStatsView};
use ferry_pin::{HeldSummary, PinError, PinManager, PinRecord};
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
        let manifest = self.handle.current_manifest_id();
        let manifest_id_hex = manifest.map(|m| hex_str(&m));
        let state = if manifest.is_some() {
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

        let pin_summary = PinManager::new(self.state_dir())
            .summary()
            .unwrap_or_else(|_| HeldSummary::none());

        let pin_view = PinView {
            state: pin_summary.state,
            holding: pin_summary.holding,
            paths: pin_summary.paths,
        };

        let held_by_peer: HashMap<String, Vec<String>> =
            pin_summary.held_by_peer.into_iter().collect();
        let held_total = pin_summary.total_held_paths;

        let peers = match AgreementLedger::new(self.state_dir()).list_folder(&self.folder_id) {
            Ok(records) => {
                let mut list = Vec::new();
                for (dev, rec) in records {
                    list.push(PeerStatusView {
                        device_id: hex_str(&dev),
                        last_agreed_manifest_id: Some(hex_str(&rec.manifest_id)),
                        agreed_at: Some(ferry_platform::time::fmt_rfc3339(rec.agreed_sec)),
                        connectivity: self.handle.peer_connectivity(&dev).to_string(),
                    });
                }
                list.sort_by(|a, b| a.device_id.cmp(&b.device_id));
                list
            }
            Err(_) => Vec::new(),
        };

        let conflicts = ferry_sync_engine::list_conflicts(&self.state_dir()).map_or(0, |c| c.len());

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
        let mut base_agreements = BTreeMap::new();
        if let Ok(records) = AgreementLedger::new(self.state_dir()).list_folder(&self.folder_id) {
            for (dev, rec) in records {
                base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
            }
        }

        PinManager::new(self.state_dir()).start_session(
            paths,
            std::process::id(),
            &self.device_hex,
            base_agreements,
        )
    }

    /// Release an active session pin.
    pub fn release_pin(&self) -> Result<bool, PinError> {
        PinManager::new(self.state_dir()).stop_session()
    }

    /// Trigger an audit-grade filesystem rescan.
    pub fn trigger_scan(&self) {
        self.handle.trigger_scan();
    }

    /// List recorded conflicts.
    pub fn list_conflicts(
        &self,
    ) -> Result<Vec<ferry_sync_engine::ConflictEntry>, ferry_sync_engine::LogError> {
        ferry_sync_engine::list_conflicts(&self.state_dir())
    }
}
