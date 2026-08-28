//! Shared daemon state, multi-folder supervision, and engine snapshot generation.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rand::rngs::StdRng;
use rand::SeedableRng;

use ferry_crypto::identity::DeviceIdentity;
use ferry_folder::folder::{
    create_folder, open_folder, save_settings, write_default_ignore_if_absent, Settings,
    SETTINGS_FORMAT_VERSION,
};
use ferry_ipc::backend::{FolderInfo, OpError};
use ferry_ipc::{DaemonMessage, EngineSnapshot, PeerStatusView, PinView, ScanStatsView};
use ferry_pin::{HeldSummary, PinError, PinManager, PinRecord};
use ferry_store::agreement::AgreementLedger;
use ferry_store::format::hex as hex_str;
use ferry_store::format::unhex;
use ferry_sync::{EngineConfig, EngineHandle, SyncEngine, Transport};

use crate::registry::{default_folders_toml_path, ferry_home, FolderRegistry};

/// Metadata and handle for an active folder supervised by the daemon.
#[derive(Clone)]
pub struct ManagedFolder {
    pub folder_id: [u8; 16],
    pub folder_id_hex: String,
    pub path: PathBuf,
    pub store_dir: PathBuf,
    pub handle: EngineHandle,
    pub listen_addr: Option<std::net::SocketAddr>,
}

/// Shared runtime state of the daemon, supervising multiple isolated `SyncEngine` instances.
pub struct DaemonState {
    home_dir: PathBuf,
    identity: DeviceIdentity,
    device_hex: String,
    transport: Arc<dyn Transport>,
    managed_folders: RwLock<HashMap<[u8; 16], ManagedFolder>>,
    active_folder_id: RwLock<Option<[u8; 16]>>,
    registry_path: PathBuf,
    broadcast_tx: tokio::sync::broadcast::Sender<DaemonMessage>,
}

impl DaemonState {
    /// Create a new `DaemonState` initialized with an existing engine handle.
    pub fn new(
        handle: EngineHandle,
        store_dir: PathBuf,
        tree_dir: PathBuf,
        folder_id: [u8; 16],
        identity: DeviceIdentity,
        broadcast_tx: tokio::sync::broadcast::Sender<DaemonMessage>,
    ) -> Self {
        let device_hex = hex_str(identity.public());
        let home_dir = ferry_home();
        let registry_path = default_folders_toml_path();

        let managed = ManagedFolder {
            folder_id,
            folder_id_hex: hex_str(&folder_id),
            path: tree_dir.clone(),
            store_dir,
            handle,
            listen_addr: None,
        };
        let mut map = HashMap::new();
        map.insert(folder_id, managed);

        let mut reg = FolderRegistry::load_from_file(&registry_path).unwrap_or_default();
        reg.register(hex_str(&folder_id), tree_dir);
        reg.switch(&hex_str(&folder_id));
        let _ = reg.save_to_file(&registry_path);

        Self {
            home_dir,
            identity,
            device_hex,
            transport: Arc::new(ferry_sync::TcpTransport),
            managed_folders: RwLock::new(map),
            active_folder_id: RwLock::new(Some(folder_id)),
            registry_path,
            broadcast_tx,
        }
    }

    /// Create a new multi-folder `DaemonState` with a specific home directory and transport.
    #[must_use]
    pub fn with_home_and_transport(
        home_dir: PathBuf,
        identity: DeviceIdentity,
        transport: Arc<dyn Transport>,
        broadcast_tx: tokio::sync::broadcast::Sender<DaemonMessage>,
    ) -> Self {
        let device_hex = hex_str(identity.public());
        let registry_path = home_dir.join("folders.toml");

        let state = Self {
            home_dir,
            identity,
            device_hex,
            transport,
            managed_folders: RwLock::new(HashMap::new()),
            active_folder_id: RwLock::new(None),
            registry_path,
            broadcast_tx,
        };

        if let Ok(reg) = FolderRegistry::load_from_file(&state.registry_path) {
            for entry in reg.list() {
                if let Some(fid) = unhex::<16>(&entry.id) {
                    if entry.path.exists() && entry.path.join(".ferry").exists() {
                        let _ = state.start_folder_engine(fid, entry.path.clone());
                    }
                }
            }
            if let Some(active_id) = reg.active_folder_id.as_deref().and_then(unhex::<16>) {
                *state.active_folder_id.write().unwrap() = Some(active_id);
            } else if let Some(first) = reg.list().first().and_then(|e| unhex::<16>(&e.id)) {
                *state.active_folder_id.write().unwrap() = Some(first);
            }
        }

        state
    }

    fn start_folder_engine(
        &self,
        folder_id: [u8; 16],
        path: PathBuf,
    ) -> Result<ManagedFolder, OpError> {
        let opened = open_folder(&path, &self.identity)
            .map_err(|e| OpError::new(e.code, e.message, e.hint))?;
        let poly = ferry_store::chunker::ValidatedPoly::new(opened.poly)
            .map_err(|e| OpError::bad_request(e.to_string(), "invalid poly"))?;
        let folder_id_hex = hex_str(&folder_id);

        let state_dir = path.join(".ferry");
        let mut connect_to = None;
        if let Ok(rd) = std::fs::read_dir(state_dir.join("peers")) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("addr") {
                    if let Ok(content) = std::fs::read_to_string(&p) {
                        if let Ok(addr) = content.trim().parse::<std::net::SocketAddr>() {
                            connect_to = Some(addr);
                            break;
                        }
                    }
                }
            }
        }

        let bind_addr = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
        let cfg = EngineConfig {
            tag: format!("folder-{folder_id_hex}"),
            store_dir: path.clone(),
            tree_dir: path.clone(),
            poly,
            folder_id,
            poll_interval: Duration::from_millis(100),
            opportunistic_every: 1,
            bind_addr,
            connect_to,
            expected_peer_id: None,
            pin_state_dir: Some(path.join(".ferry")),
            quiet: false,
        };
        let mut engine = SyncEngine::new(cfg, Arc::clone(&self.transport))
            .map_err(|e| OpError::internal(e.to_string(), "engine init failed"))?;
        engine.set_identity(self.identity.clone());
        let listen_addr = engine.listen_addr();
        if let Some(addr) = listen_addr {
            let _ = std::fs::create_dir_all(&state_dir);
            let _ = std::fs::write(state_dir.join("listen.addr"), addr.to_string());
        }
        let handle = engine.start();
        eprintln!(
            "Managed folder engine started: id={}, path={}, listen={:?}, connect_to={:?}",
            folder_id_hex,
            path.display(),
            listen_addr,
            connect_to
        );

        let managed = ManagedFolder {
            folder_id,
            folder_id_hex,
            path: path.clone(),
            store_dir: path,
            handle,
            listen_addr,
        };
        self.managed_folders
            .write()
            .unwrap()
            .insert(folder_id, managed.clone());
        Ok(managed)
    }

    /// Register a folder path to sync under the daemon. If not yet initialized, bootstraps `.ferry`.
    pub fn register_folder(&self, path: PathBuf) -> Result<FolderInfo, OpError> {
        let abs_path = if path.is_relative() {
            std::env::current_dir().map_or_else(|_| path.clone(), |cwd| cwd.join(&path))
        } else {
            path.clone()
        };
        if !abs_path.exists() {
            std::fs::create_dir_all(&abs_path)
                .map_err(|e| OpError::new("io", e.to_string(), "cannot create directory"))?;
        }

        let opened = if abs_path.join(".ferry").join("config").exists() {
            open_folder(&abs_path, &self.identity)
                .map_err(|e| OpError::new(e.code, e.message, e.hint))?
        } else {
            let fid = rand::random::<[u8; 16]>();
            let mut rng = StdRng::from_entropy();
            let poly = ferry_store::chunker::generate_polynomial(&mut rng);
            let (store, _fmk) = create_folder(&abs_path, &self.identity, fid, poly)
                .map_err(|e| OpError::new(e.code, e.message, e.hint))?;
            store
                .flush()
                .map_err(|e| OpError::new("store", e.to_string(), "store flush failed"))?;
            store
                .write_index_snapshot()
                .map_err(|e| OpError::new("store", e.to_string(), "index snapshot failed"))?;
            let settings = Settings {
                format_version: SETTINGS_FORMAT_VERSION,
                folder_id: hex_str(&fid),
                honor_gitignore: true,
                presets: Vec::new(),
                overrides: Vec::new(),
            };
            save_settings(&abs_path, &settings)
                .map_err(|e| OpError::new(e.code, e.message, e.hint))?;
            let _ = write_default_ignore_if_absent(&abs_path);
            open_folder(&abs_path, &self.identity)
                .map_err(|e| OpError::new(e.code, e.message, e.hint))?
        };

        let folder_id = opened.folder_id;
        let folder_id_hex = hex_str(&folder_id);

        if !self.managed_folders.read().unwrap().contains_key(&folder_id) {
            let poly = ferry_store::chunker::ValidatedPoly::new(opened.poly)
                .map_err(|e| OpError::bad_request(e.to_string(), "invalid poly"))?;

            let state_dir = abs_path.join(".ferry");
            let mut connect_to = None;
            if let Ok(rd) = std::fs::read_dir(state_dir.join("peers")) {
                for entry in rd.flatten() {
                    let p = entry.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("addr") {
                        if let Ok(content) = std::fs::read_to_string(&p) {
                            if let Ok(addr) = content.trim().parse::<std::net::SocketAddr>() {
                                connect_to = Some(addr);
                                break;
                            }
                        }
                    }
                }
            }

            let bind_addr = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
            let cfg = EngineConfig {
                tag: format!("folder-{folder_id_hex}"),
                store_dir: abs_path.clone(),
                tree_dir: abs_path.clone(),
                poly,
                folder_id,
                poll_interval: Duration::from_millis(100),
                opportunistic_every: 1,
                bind_addr,
                connect_to,
                expected_peer_id: None,
                pin_state_dir: Some(abs_path.join(".ferry")),
                quiet: false,
            };
            let mut engine = SyncEngine::new(cfg, Arc::clone(&self.transport))
                .map_err(|e| OpError::internal(e.to_string(), "engine init failed"))?;
            engine.set_identity(self.identity.clone());
            let listen_addr = engine.listen_addr();
            if let Some(addr) = listen_addr {
                let _ = std::fs::create_dir_all(&state_dir);
                let _ = std::fs::write(state_dir.join("listen.addr"), addr.to_string());
            }
            let handle = engine.start();
            eprintln!(
                "register_folder engine started: id={}, path={}, listen={:?}, connect_to={:?}",
                folder_id_hex,
                abs_path.display(),
                listen_addr,
                connect_to
            );

            let managed = ManagedFolder {
                folder_id,
                folder_id_hex: folder_id_hex.clone(),
                path: abs_path.clone(),
                store_dir: abs_path.clone(),
                handle,
                listen_addr,
            };
            self.managed_folders
                .write()
                .unwrap()
                .insert(folder_id, managed);
        }

        *self.active_folder_id.write().unwrap() = Some(folder_id);

        let mut reg = FolderRegistry::load_from_file(&self.registry_path).unwrap_or_default();
        reg.register(folder_id_hex.clone(), abs_path.clone());
        reg.switch(&folder_id_hex);
        let _ = reg.save_to_file(&self.registry_path);

        let snap = self.snapshot();
        self.broadcast(DaemonMessage::StateChanged {
            state: snap.state.clone(),
            manifest_id: snap.manifest_id.unwrap_or_default(),
            agreed_id: None,
            pending_changes: snap.pending_changes,
            stats: Some(snap.scanned),
        });

        Ok(FolderInfo {
            id: folder_id_hex,
            path: abs_path,
            active: true,
            state: Some(snap.state),
        })
    }

    /// Unregister a folder by its hex ID and terminate its engine instance.
    pub fn unregister_folder(&self, folder_id_hex: &str) -> Result<(), OpError> {
        let fid = unhex::<16>(folder_id_hex)
            .ok_or_else(|| OpError::bad_request("invalid folder_id hex", "32 hex characters required"))?;

        let removed = self.managed_folders.write().unwrap().remove(&fid);
        if let Some(managed) = removed {
            managed.handle.shutdown();
        }

        {
            let mut active_lock = self.active_folder_id.write().unwrap();
            if *active_lock == Some(fid) {
                let next_active = self.managed_folders.read().unwrap().keys().copied().next();
                *active_lock = next_active;
            }
        }

        let mut reg = FolderRegistry::load_from_file(&self.registry_path).unwrap_or_default();
        reg.unregister(folder_id_hex);
        let _ = reg.save_to_file(&self.registry_path);

        let snap = self.snapshot();
        self.broadcast(DaemonMessage::StateChanged {
            state: snap.state.clone(),
            manifest_id: snap.manifest_id.unwrap_or_default(),
            agreed_id: None,
            pending_changes: snap.pending_changes,
            stats: Some(snap.scanned),
        });

        Ok(())
    }

    /// Switch active folder context by hex ID.
    pub fn switch_folder(&self, folder_id_hex: &str) -> Result<EngineSnapshot, OpError> {
        let fid = unhex::<16>(folder_id_hex)
            .ok_or_else(|| OpError::bad_request("invalid folder_id hex", "32 hex characters required"))?;

        let exists = self.managed_folders.read().unwrap().contains_key(&fid);
        if !exists {
            let reg = FolderRegistry::load_from_file(&self.registry_path).unwrap_or_default();
            if let Some(entry) = reg.get(folder_id_hex) {
                let _ = self.start_folder_engine(fid, entry.path.clone())?;
            } else {
                return Err(OpError::not_found("folder not found", "register the folder first"));
            }
        }

        *self.active_folder_id.write().unwrap() = Some(fid);

        let mut reg = FolderRegistry::load_from_file(&self.registry_path).unwrap_or_default();
        reg.switch(folder_id_hex);
        let _ = reg.save_to_file(&self.registry_path);

        let snap = self.snapshot();
        self.broadcast(DaemonMessage::StateChanged {
            state: snap.state.clone(),
            manifest_id: snap.manifest_id.clone().unwrap_or_default(),
            agreed_id: None,
            pending_changes: snap.pending_changes,
            stats: Some(snap.scanned),
        });

        Ok(snap)
    }

    /// Return a list of all registered and supervised folders.
    #[must_use]
    pub fn list_folders(&self) -> Vec<FolderInfo> {
        let active_id = *self.active_folder_id.read().unwrap();
        let managed = self.managed_folders.read().unwrap();

        let mut result = Vec::new();
        for (fid, m) in managed.iter() {
            let is_active = Some(*fid) == active_id;
            let manifest = m.handle.current_manifest_id();
            let state_str = if manifest.is_some() {
                "idle".to_string()
            } else {
                "initializing".to_string()
            };
            result.push(FolderInfo {
                id: m.folder_id_hex.clone(),
                path: m.path.clone(),
                active: is_active,
                state: Some(state_str),
            });
        }

        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }

    /// Return all active managed folder handles and metadata.
    #[must_use]
    pub fn managed_folders(&self) -> Vec<ManagedFolder> {
        self.managed_folders.read().unwrap().values().cloned().collect()
    }

    pub fn handle(&self) -> EngineHandle {
        let active_id = *self.active_folder_id.read().unwrap();
        if let Some(fid) = active_id {
            let managed = self.managed_folders.read().unwrap();
            if let Some(m) = managed.get(&fid) {
                return m.handle.clone();
            }
        }
        panic!("no active engine in DaemonState");
    }

    #[must_use]
    pub fn active_folder_id(&self) -> Option<[u8; 16]> {
        *self.active_folder_id.read().unwrap()
    }

    #[must_use]
    pub fn store_dir(&self) -> PathBuf {
        let active_id = *self.active_folder_id.read().unwrap();
        if let Some(fid) = active_id {
            let managed = self.managed_folders.read().unwrap();
            if let Some(m) = managed.get(&fid) {
                return m.store_dir.clone();
            }
        }
        self.home_dir.clone()
    }

    #[must_use]
    pub fn tree_dir(&self) -> PathBuf {
        let active_id = *self.active_folder_id.read().unwrap();
        if let Some(fid) = active_id {
            let managed = self.managed_folders.read().unwrap();
            if let Some(m) = managed.get(&fid) {
                return m.path.clone();
            }
        }
        self.home_dir.clone()
    }

    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.store_dir().join(".ferry")
    }

    #[must_use]
    pub fn store_dir_for(&self, folder_id: &[u8; 16]) -> Option<PathBuf> {
        let managed = self.managed_folders.read().unwrap();
        managed.get(folder_id).map(|m| m.store_dir.clone())
    }

    #[must_use]
    pub fn state_dir_for(&self, folder_id: &[u8; 16]) -> Option<PathBuf> {
        let managed = self.managed_folders.read().unwrap();
        managed.get(folder_id).map(|m| {
            if m.store_dir.ends_with(".ferry") {
                m.store_dir.clone()
            } else {
                m.store_dir.join(".ferry")
            }
        })
    }

    #[must_use]
    pub fn folder_id(&self) -> [u8; 16] {
        let active_id = *self.active_folder_id.read().unwrap();
        active_id.unwrap_or([0u8; 16])
    }

    #[must_use]
    pub fn device_hex(&self) -> &str {
        &self.device_hex
    }

    #[must_use]
    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    #[must_use]
    pub fn broadcast_tx(&self) -> &tokio::sync::broadcast::Sender<DaemonMessage> {
        &self.broadcast_tx
    }

    /// Broadcast a daemon message to all connected IPC clients.
    pub fn broadcast(&self, msg: DaemonMessage) {
        let _ = self.broadcast_tx.send(msg);
    }

    /// Generate an `EngineSnapshot` capturing the current state of the active folder and engine.
    #[must_use]
    pub fn snapshot(&self) -> EngineSnapshot {
        let active_id = *self.active_folder_id.read().unwrap();
        if let Some(fid) = active_id {
            if let Some(snap) = self.snapshot_for(&fid) {
                return snap;
            }
        }
        EngineSnapshot::new("", "", &self.device_hex, "idle")
    }

    /// Generate an `EngineSnapshot` for a specific folder ID.
    #[must_use]
    pub fn snapshot_for(&self, folder_id: &[u8; 16]) -> Option<EngineSnapshot> {
        let managed = self.managed_folders.read().unwrap();
        let m = managed.get(folder_id)?;

        let manifest = m.handle.current_manifest_id();
        let manifest_id_hex = manifest.map(|m_id| hex_str(&m_id));
        let state = if manifest.is_some() {
            "idle".to_string()
        } else {
            "initializing".to_string()
        };

        let scanned = m
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

        let pending_changes = m.handle.pending_changes();
        let state_dir = m.store_dir.join(".ferry");

        let pin_summary = PinManager::new(&state_dir)
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

        let peers = match AgreementLedger::new(&state_dir).list_folder(folder_id) {
            Ok(records) => {
                let mut list = Vec::new();
                for (dev, rec) in records {
                    list.push(PeerStatusView {
                        device_id: hex_str(&dev),
                        last_agreed_manifest_id: Some(hex_str(&rec.manifest_id)),
                        agreed_at: Some(ferry_platform::time::fmt_rfc3339(rec.agreed_sec)),
                        connectivity: m.handle.peer_connectivity(&dev).to_string(),
                    });
                }
                list.sort_by(|a, b| a.device_id.cmp(&b.device_id));
                list
            }
            Err(_) => Vec::new(),
        };

        let conflicts = ferry_sync_engine::list_conflicts(&state_dir).map_or(0, |c| c.len());

        Some(EngineSnapshot {
            folder: m.path.display().to_string(),
            folder_id: hex_str(folder_id),
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
        })
    }

    /// Start a session pin on the active folder.
    pub fn start_pin(
        &self,
        paths: Vec<String>,
        duration_hours: Option<u64>,
    ) -> Result<PinRecord, PinError> {
        let active_id = *self.active_folder_id.read().unwrap();
        let fid = active_id.ok_or(PinError::PinActive { pid: 0 })?;
        let managed = self.managed_folders.read().unwrap();
        let m = managed.get(&fid).ok_or(PinError::PinActive { pid: 0 })?;
        let state_dir = m.store_dir.join(".ferry");

        let mut base_agreements = BTreeMap::new();
        if let Ok(records) = AgreementLedger::new(&state_dir).list_folder(&fid) {
            for (dev, rec) in records {
                base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
            }
        }

        let duration_secs = duration_hours.map(|h| h * 3600);
        PinManager::new(&state_dir).start_session_with_duration(
            paths,
            std::process::id(),
            &self.device_hex,
            base_agreements,
            duration_secs,
        )
    }

    /// Release an active session pin on the active folder.
    pub fn release_pin(&self) -> Result<bool, PinError> {
        let active_id = *self.active_folder_id.read().unwrap();
        let fid = active_id.ok_or(PinError::PinActive { pid: 0 })?;
        let managed = self.managed_folders.read().unwrap();
        let m = managed.get(&fid).ok_or(PinError::PinActive { pid: 0 })?;
        let state_dir = m.store_dir.join(".ferry");
        PinManager::new(&state_dir).stop_session()
    }

    /// Trigger an audit-grade filesystem rescan on the active folder.
    pub fn trigger_scan(&self) {
        let active_id = *self.active_folder_id.read().unwrap();
        if let Some(fid) = active_id {
            let managed = self.managed_folders.read().unwrap();
            if let Some(m) = managed.get(&fid) {
                m.handle.trigger_scan();
            }
        }
    }

    /// Trigger a scan across all registered folders.
    pub fn trigger_scan_all(&self) {
        let managed = self.managed_folders.read().unwrap();
        for m in managed.values() {
            m.handle.trigger_scan();
        }
    }

    /// List recorded conflicts in the active folder.
    pub fn list_conflicts(
        &self,
    ) -> Result<Vec<ferry_sync_engine::ConflictEntry>, ferry_sync_engine::LogError> {
        let active_id = *self.active_folder_id.read().unwrap();
        if let Some(fid) = active_id {
            let managed = self.managed_folders.read().unwrap();
            if let Some(m) = managed.get(&fid) {
                return ferry_sync_engine::list_conflicts(&m.store_dir.join(".ferry"));
            }
        }
        Ok(Vec::new())
    }
}
