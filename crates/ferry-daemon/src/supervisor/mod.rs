pub mod engine;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_crypto::identity::DeviceIdentity;
use ferry_folder::inventory::{FolderInventory, FolderRecord};
use ferry_ipc::backend::{OpError, UiEvent};
use ferry_ipc::protocol::EngineSnapshot;
use ferry_sync::EngineHandle;

pub use engine::{EngineSpawnOptions, FolderEngine, FolderId};

#[derive(Debug, Clone)]
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

pub type SupervisedEngine = FolderEngine;

pub struct Supervisor {
    home: PathBuf,
    identity: DeviceIdentity,
    engines: HashMap<FolderId, FolderEngine>,
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
        ) = match ferry_iroh::IrohTransport::new(ferry_iroh::IrohConfig {
            device_identity: Some(identity.clone()),
            ..Default::default()
        }) {
            Ok(t) => {
                let arc = Arc::new(t);
                (arc.clone(), Some(arc))
            }
            Err(_) => (Arc::new(ferry_sync::TcpTransport), None),
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
        mut options: EngineSpawnOptions,
    ) -> Result<FolderEngine, SupervisorError> {
        if options.bind_addr.is_none() && self.iroh_transport.is_some() {
            options.bind_addr = Some("127.0.0.1:0".parse().unwrap());
        }
        FolderEngine::start(
            folder_path,
            &self.identity,
            Arc::clone(&self.transport),
            options,
            self.broadcast_tx.clone(),
        )
    }

    fn default_options(&self) -> EngineSpawnOptions {
        let bind_addr = if self.iroh_transport.is_some() {
            Some("127.0.0.1:0".parse().unwrap())
        } else {
            None
        };
        EngineSpawnOptions {
            bind_addr,
            connect_to: None,
            opportunistic_every: Some(50),
            poll_interval: Some(Duration::from_millis(200)),
        }
    }

    pub fn spawn_engines(&mut self) -> Result<(), SupervisorError> {
        let records = self.inventory().list().map_err(|e| SupervisorError {
            code: e.code.to_string(),
            message: e.message.clone(),
        })?;
        let options = self.default_options();
        for rec in records {
            let id = rec.folder_id.clone();
            if self.engines.contains_key(&id) {
                continue;
            }
            let engine = FolderEngine::start_with_record(
                rec,
                &self.identity,
                Arc::clone(&self.transport),
                options.clone(),
                self.broadcast_tx.clone(),
            )?;
            self.engines.insert(id, engine);
        }
        Ok(())
    }

    pub fn handle_register(&mut self, path: PathBuf) -> Result<FolderRecord, OpError> {
        let record = self.inventory().register(&path).map_err(OpError::from)?;
        let id = record.folder_id.clone();
        let options = self.default_options();
        let engine = FolderEngine::start_with_record(
            record.clone(),
            &self.identity,
            Arc::clone(&self.transport),
            options,
            self.broadcast_tx.clone(),
        )
        .map_err(|e| OpError::new(e.code, e.message, "check daemon log"))?;
        self.engines.insert(id, engine);
        let _ = self.broadcast_tx.send(UiEvent::FolderRegistered {
            path: path.display().to_string(),
        });
        Ok(record)
    }

    pub fn handle_remove(&mut self, folder_id: &str) -> Result<(), OpError> {
        self.inventory()
            .unregister(folder_id)
            .map_err(OpError::from)?;
        if let Some(mut engine) = self.engines.remove(folder_id) {
            engine.shutdown();
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
        let engine = if let Some(ref fid) = folder_id {
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
        Ok(engine.snapshot())
    }

    pub fn engines_map(&self) -> &HashMap<FolderId, FolderEngine> {
        &self.engines
    }

    pub fn get_engine_handle(&self, folder_id: &str) -> Option<Arc<EngineHandle>> {
        self.engines.get(folder_id).map(|e| Arc::clone(e.handle()))
    }

    pub fn tick(&mut self) {
        for engine in self.engines.values_mut() {
            engine.tick();
        }
    }

    pub fn shutdown(&mut self) {
        for (_, mut engine) in self.engines.drain() {
            engine.shutdown();
        }
    }

    pub fn wait_for_manifests(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let mut all_ready = true;
            for e in self.engines.values() {
                if e.handle().current_manifest_id().is_none() {
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
