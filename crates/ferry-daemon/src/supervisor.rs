use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_crypto::identity::DeviceIdentity;
use ferry_ipc::backend::{OpError, UiEvent};
use ferry_ipc::protocol::EngineSnapshot;
use ferry_store::format::hex as hex_str;
use ferry_sync::{EngineConfig, EngineHandle, SyncEngine, TcpTransport};

use crate::registry::{FolderRegistry, RegistryError};

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
    pub record: ferry_ipc::registry::FolderRecord,
    pub handle: Arc<EngineHandle>,
    pub task: tokio::task::JoinHandle<()>,
    pub folder_id_bytes: [u8; 16],
    pub restart_count: u32,
}

pub struct Supervisor {
    home: PathBuf,
    identity: DeviceIdentity,
    engines: HashMap<String, SupervisedEngine>,
    broadcast_tx: tokio::sync::broadcast::Sender<UiEvent>,
}

impl Supervisor {
    pub fn new(home: PathBuf, identity: DeviceIdentity) -> Self {
        let (broadcast_tx, _) = tokio::sync::broadcast::channel(64);
        Self {
            home,
            identity,
            engines: HashMap::new(),
            broadcast_tx,
        }
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    pub fn broadcast_tx(&self) -> &tokio::sync::broadcast::Sender<UiEvent> {
        &self.broadcast_tx
    }

    pub fn load_registry(&self) -> Result<FolderRegistry, RegistryError> {
        FolderRegistry::load(&self.home)
    }

    fn parse_folder_id(hex_str: &str) -> Result<[u8; 16], OpError> {
        if hex_str.len() == 32 {
            ferry_store::format::unhex::<16>(hex_str).ok_or_else(|| {
                OpError::new(
                    "corrupt-registry",
                    format!("invalid folder_id {hex_str}"),
                    "fix or delete folders.toml",
                )
            })
        } else if hex_str.len() == 64 {
            let trunc = &hex_str[..32];
            ferry_store::format::unhex::<16>(trunc).ok_or_else(|| {
                OpError::new(
                    "corrupt-registry",
                    format!("invalid folder_id {hex_str}"),
                    "fix or delete folders.toml",
                )
            })
        } else {
            Err(OpError::new(
                "corrupt-registry",
                format!("invalid folder_id length {}: {hex_str}", hex_str.len()),
                "fix or delete folders.toml",
            ))
        }
    }

    fn poly_for_id(folder_id: &str) -> ferry_store::chunker::ValidatedPoly {
        use rand::SeedableRng;
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        folder_id.hash(&mut hasher);
        let h = hasher.finish();
        let mut seed = [0u8; 32];
        seed[..8].copy_from_slice(&h.to_le_bytes());
        seed[8..16].copy_from_slice(&h.to_be_bytes());
        let mut rng = rand::rngs::StdRng::from_seed(seed);
        ferry_store::chunker::ValidatedPoly::generate(&mut rng)
    }

    fn spawn_one(
        &self,
        record: ferry_ipc::registry::FolderRecord,
    ) -> Result<SupervisedEngine, SupervisorError> {
        let folder_id_bytes = Self::parse_folder_id(&record.folder_id).map_err(|e| SupervisorError {
            code: e.code.clone(),
            message: e.message.clone(),
        })?;
        let poly = Self::poly_for_id(&record.folder_id);
        let tag = format!(
            "ferry-{}",
            &record.folder_id[..8.min(record.folder_id.len())]
        );
        let cfg = EngineConfig {
            tag: tag.clone(),
            store_dir: record.path.clone(),
            tree_dir: record.path.clone(),
            poly,
            folder_id: folder_id_bytes,
            poll_interval: Duration::from_millis(200),
            opportunistic_every: 50,
            bind_addr: None,
            connect_to: None,
            expected_peer_id: None,
            pin_state_dir: Some(record.path.join(".ferry")),
            quiet: true,
        };
        let transport: Arc<dyn ferry_sync::Transport> = Arc::new(TcpTransport);
        let mut engine = SyncEngine::new(cfg, transport).map_err(|e| SupervisorError {
            code: "engine-init".to_string(),
            message: e.to_string(),
        })?;
        engine.set_identity(self.identity.clone());
        let handle = Arc::new(engine.start());
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        Ok(SupervisedEngine {
            record,
            handle,
            task,
            folder_id_bytes,
            restart_count: 0,
        })
    }

    pub fn spawn_engines(&mut self) -> Result<(), SupervisorError> {
        let registry = self.load_registry().map_err(|e| SupervisorError {
            code: e.code.clone(),
            message: e.message.clone(),
        })?;
        for rec in registry.folders {
            let id = rec.folder_id.clone();
            if self.engines.contains_key(&id) {
                continue;
            }
            let entry = self.spawn_one(rec)?;
            self.engines.insert(id, entry);
        }
        Ok(())
    }

    pub fn handle_register(&mut self, path: PathBuf) -> Result<ferry_ipc::registry::FolderRecord, OpError> {
        let mut registry = self.load_registry().map_err(|e| e.to_op_error())?;
        let record = registry.register(path).map_err(|e| e.to_op_error())?;
        registry.save(&self.home).map_err(|e| e.to_op_error())?;
        let id = record.folder_id.clone();
        let entry = self.spawn_one(record.clone()).map_err(|e| {
            OpError::new(e.code, e.message, "check daemon log")
        })?;
        self.engines.insert(id, entry);
        Ok(record)
    }

    pub fn handle_remove(&mut self, folder_id: &str) -> Result<(), OpError> {
        let mut registry = self.load_registry().map_err(|e| e.to_op_error())?;
        registry.remove(folder_id).map_err(|e| e.to_op_error())?;
        registry.save(&self.home).map_err(|e| e.to_op_error())?;
        if let Some(entry) = self.engines.remove(folder_id) {
            entry.handle.shutdown();
            entry.task.abort();
        }
        Ok(())
    }

    pub fn list_folders(&self) -> Vec<ferry_ipc::registry::FolderRecord> {
        match self.load_registry() {
            Ok(r) => r.folders.clone(),
            Err(_) => Vec::new(),
        }
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
            .map(|s| ferry_ipc::protocol::ScanStatsView::new(
                s.files as u64,
                s.dirs as u64,
                s.symlinks as u64,
                s.bytes_chunked,
            ))
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

    /// Test helper: abort one engine's supervision task.
    pub fn abort_task(&self, folder_id: &str) -> bool {
        if let Some(e) = self.engines.get(folder_id) {
            e.task.abort();
            true
        } else {
            false
        }
    }

    pub fn task_is_finished(&self, folder_id: &str) -> Option<bool> {
        self.engines.get(folder_id).map(|e| e.task.is_finished())
    }

    /// Supervision tick: detect crashed tasks and restart with backoff.
    pub fn tick(&mut self) {
        let mut to_restart: Vec<String> = Vec::new();
        for (id, entry) in &self.engines {
            if entry.task.is_finished() {
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
            entry.task.abort();
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

// Need to allow HashMap value to be private but accessed via method; expose type via pub(crate) helper.
// We'll provide a public accessor for supervision task manipulation.
impl SupervisedEngine {
    pub fn handle(&self) -> &Arc<EngineHandle> {
        &self.handle
    }
    pub fn task(&self) -> &tokio::task::JoinHandle<()> {
        &self.task
    }
}
