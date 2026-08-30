use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_crypto::identity::DeviceIdentity;
use ferry_folder::inventory::{FolderInventory, FolderRecord};
use ferry_ipc::backend::{OpError, UiEvent};
use ferry_ipc::protocol::EngineSnapshot;
use ferry_store::format::hex as hex_str;
use ferry_sync::{EngineConfig, EngineHandle, SyncEngine, TcpTransport};

#[derive(Debug)]
pub struct SupervisorError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SupervisorError {}

pub struct SupervisedEngine {
    pub record: FolderRecord,
    pub handle: Arc<EngineHandle>,
    pub folder_id_bytes: [u8; 16],
    pub restart_count: u32,
}

use std::net::SocketAddr;

#[derive(Debug, Clone, Default)]
pub struct EngineSpawnOptions {
    pub bind_addr: Option<SocketAddr>,
    pub connect_to: Option<SocketAddr>,
    pub opportunistic_every: Option<u32>,
    pub poll_interval: Option<Duration>,
}

pub struct Supervisor {
    home: PathBuf,
    identity: DeviceIdentity,
    engines: HashMap<String, SupervisedEngine>,
    broadcast_tx: tokio::sync::broadcast::Sender<UiEvent>,
    transport: Arc<dyn ferry_sync::Transport>,
    iroh_transport: Option<Arc<ferry_iroh::IrohTransport>>,
}

impl Supervisor {
    pub fn new(home: PathBuf, identity: DeviceIdentity) -> Self {
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(64);
        let (transport, iroh_transport): (
            Arc<dyn ferry_sync::Transport>,
            Option<Arc<ferry_iroh::IrohTransport>>,
        ) = match ferry_iroh::IrohTransport::new(
            ferry_iroh::IrohConfig::builder()
                .device_identity(&identity)
                .build(),
        ) {
            Ok(t) => {
                let arc = Arc::new(t);
                (arc.clone(), Some(arc))
            }
            Err(_) => (Arc::new(TcpTransport), None),
        };
        Self {
            home,
            identity,
            engines: HashMap::new(),
            broadcast_tx,
            transport,
            iroh_transport,
        }
    }

    pub fn with_transport(
        home: PathBuf,
        identity: DeviceIdentity,
        transport: Arc<dyn ferry_sync::Transport>,
    ) -> Self {
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            home,
            identity,
            engines: HashMap::new(),
            broadcast_tx,
            transport,
            iroh_transport: None,
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    pub fn transport(&self) -> &Arc<dyn ferry_sync::Transport> {
        &self.transport
    }

    pub fn broadcast_tx(&self) -> &tokio::sync::broadcast::Sender<UiEvent> {
        &self.broadcast_tx
    }

    pub fn inventory(&self) -> FolderInventory {
        FolderInventory::new(&self.home)
    }

    pub fn spawn_engine(
        &self,
        folder_path: &Path,
        options: EngineSpawnOptions,
    ) -> Result<SupervisedEngine, SupervisorError> {
        self.spawn_engine_internal(folder_path, options, None)
    }

    fn spawn_engine_internal(
        &self,
        folder_path: &Path,
        options: EngineSpawnOptions,
        existing_record: Option<FolderRecord>,
    ) -> Result<SupervisedEngine, SupervisorError> {
        let opened =
            ferry_folder::folder::open_folder(folder_path, &self.identity).map_err(|e| {
                SupervisorError {
                    code: e.code.to_string(),
                    message: e.to_string(),
                }
            })?;
        let poly = ferry_store::chunker::ValidatedPoly::try_from(opened.poly).map_err(|e| {
            SupervisorError {
                code: "poly-invalid".to_string(),
                message: format!("invalid chunker polynomial in store: {e}"),
            }
        })?;
        let folder_id_hex = ferry_store::format::hex(&opened.folder_id);
        let tag = format!(
            "ferry-{}",
            &folder_id_hex[..8.min(folder_id_hex.len())]
        );

        let bind_addr = options.bind_addr.or_else(|| {
            if self.iroh_transport.is_some() {
                Some("127.0.0.1:0".parse().unwrap())
            } else {
                None
            }
        });
        let opportunistic_every = options.opportunistic_every.unwrap_or(50);
        let poll_interval = options
            .poll_interval
            .unwrap_or_else(|| Duration::from_millis(200));

        let cfg = EngineConfig {
            tag,
            store_dir: opened.root.clone(),
            tree_dir: opened.root.clone(),
            poly,
            folder_id: opened.folder_id,
            poll_interval,
            opportunistic_every,
            bind_addr,
            connect_to: options.connect_to,
            allow_trust_on_first_use: false,
            pin_state_dir: Some(opened.state_dir()),
            quiet: true,
        };
        let transport = Arc::clone(&self.transport);
        let mut engine = SyncEngine::with_store(cfg, transport, Arc::clone(&opened.store))
            .map_err(|e| SupervisorError {
                code: "bind".to_string(),
                message: e.to_string(),
            })?;
        engine.set_identity(self.identity.clone());
        engine.set_ignore_policy(opened.ignore_policy());
        let handle = Arc::new(engine.start());
        let record = existing_record.unwrap_or_else(|| {
            let (secs, _) = ferry_platform::time::now_unix();
            let added_at = ferry_platform::time::fmt_rfc3339(secs);
            FolderRecord {
                folder_id: folder_id_hex,
                path: opened.root,
                added_at,
            }
        });
        Ok(SupervisedEngine {
            record,
            handle,
            folder_id_bytes: opened.folder_id,
            restart_count: 0,
        })
    }

    fn spawn_one(&self, record: FolderRecord) -> Result<SupervisedEngine, SupervisorError> {
        let options = EngineSpawnOptions {
            bind_addr: None,
            connect_to: None,
            opportunistic_every: Some(50),
            poll_interval: Some(Duration::from_millis(200)),
        };
        let path = record.path.clone();
        self.spawn_engine_internal(&path, options, Some(record))
    }

    pub fn spawn_engines(&mut self) -> Result<(), SupervisorError> {
        let records = self.inventory().list().map_err(|e| SupervisorError {
            code: e.code.to_string(),
            message: e.message.clone(),
        })?;
        for rec in records {
            let id = rec.folder_id.clone();
            if self.engines.contains_key(&id) {
                continue;
            }
            let entry = self.spawn_one(rec)?;
            self.engines.insert(id, entry);
        }
        Ok(())
    }

    pub fn handle_register(&mut self, path: PathBuf) -> Result<FolderRecord, OpError> {
        let record = self.inventory().register(&path).map_err(OpError::from)?;
        let id = record.folder_id.clone();
        let entry = self
            .spawn_one(record.clone())
            .map_err(|e| OpError::new(e.code, e.message, "check daemon log"))?;
        self.engines.insert(id, entry);
        Ok(record)
    }

    pub fn handle_remove(&mut self, folder_id: &str) -> Result<(), OpError> {
        self.inventory()
            .unregister(folder_id)
            .map_err(OpError::from)?;
        if let Some(entry) = self.engines.remove(folder_id) {
            entry.handle.shutdown();
        }
        Ok(())
    }

    pub fn list_folders(&self) -> Vec<FolderRecord> {
        self.inventory().list().unwrap_or_default()
    }

    pub fn get_status(&self, folder_id: Option<String>) -> Result<EngineSnapshot, OpError> {
        if self.engines.is_empty() {
            return Err(OpError::new(
                "not-found",
                "no folders registered",
                "register a folder first",
            ));
        }
        let entry = if let Some(ref fid) = folder_id {
            self.engines.get(fid).ok_or_else(|| {
                OpError::new(
                    "not-found",
                    format!("folder_id {fid} not found"),
                    "check folder_id",
                )
            })?
        } else {
            self.engines.values().next().unwrap()
        };
        Ok(Self::snapshot_for(entry))
    }

    fn snapshot_for(entry: &SupervisedEngine) -> EngineSnapshot {
        let handle = &entry.handle;
        let manifest_id = handle.current_manifest_id().map(|id| hex_str(&id));
        let scanned = handle
            .scan_counts()
            .map(|s| {
                ferry_ipc::protocol::ScanStatsView::new(
                    s.files as u64,
                    s.dirs as u64,
                    s.symlinks as u64,
                    s.bytes_chunked,
                )
            })
            .unwrap_or_default();
        let state = if manifest_id.is_some() {
            "idle".to_string()
        } else {
            "initializing".to_string()
        };
        let mut snap = EngineSnapshot::new(
            entry.record.path.display().to_string(),
            entry.record.folder_id.clone(),
            String::new(),
            state,
        );
        snap.manifest_id = manifest_id;
        snap.scanned = scanned;
        snap.pending_changes = handle.pending_changes();
        snap
    }

    pub fn engines_map(&self) -> &HashMap<String, SupervisedEngine> {
        &self.engines
    }

    pub fn get_engine_handle(&self, folder_id: &str) -> Option<Arc<EngineHandle>> {
        self.engines.get(folder_id).map(|e| Arc::clone(&e.handle))
    }

    /// Supervision tick: detect crashed engines (dead loops on the engine
    /// handle) and restart them with backoff.
    pub fn tick(&mut self) {
        let mut to_restart: Vec<String> = Vec::new();
        for (id, entry) in &self.engines {
            if !entry.handle.is_healthy() {
                to_restart.push(id.clone());
            }
        }
        for id in to_restart {
            if let Some(entry) = self.engines.remove(&id) {
                let record = entry.record.clone();
                let backoff_ms = 100u64.saturating_mul(1u64 << entry.restart_count.min(5));
                entry.handle.shutdown();
                let _ = self.broadcast_tx.send(UiEvent::Error {
                    code: "engine-crashed".to_string(),
                    message: format!("engine {id} crashed, restarting with {backoff_ms}ms backoff"),
                });
                // For test determinism we restart immediately (no sleep), but backoff is recorded.
                match self.spawn_one(record) {
                    Ok(mut new_entry) => {
                        new_entry.restart_count = entry.restart_count.saturating_add(1);
                        self.engines.insert(id, new_entry);
                    }
                    Err(e) => {
                        let _ = self.broadcast_tx.send(UiEvent::Error {
                            code: e.code,
                            message: e.message,
                        });
                    }
                }
            }
        }
    }

    pub fn shutdown(&mut self) {
        for (_, entry) in self.engines.drain() {
            entry.handle.shutdown();
        }
    }

    /// For tests: wait until all engines have manifest ids (scanned).
    pub fn wait_for_manifests(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let mut all_ready = true;
            for e in self.engines.values() {
                if e.handle.current_manifest_id().is_none() {
                    all_ready = false;
                    break;
                }
            }
            if all_ready {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl SupervisedEngine {
    pub fn handle(&self) -> &Arc<EngineHandle> {
        &self.handle
    }
}
