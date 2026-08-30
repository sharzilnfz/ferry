use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_folder::inventory::FolderRecord;
use ferry_ipc::backend::UiEvent;
use ferry_ipc::protocol::{EngineSnapshot, ScanStatsView};
use ferry_store::format::hex as hex_str;
use ferry_sync::{EngineConfig, EngineHandle, SyncEngine};

use super::SupervisorError;

pub type FolderId = String;

#[derive(Debug, Clone, Default)]
pub struct EngineSpawnOptions {
    pub bind_addr: Option<SocketAddr>,
    pub connect_to: Option<SocketAddr>,
    pub opportunistic_every: Option<u32>,
    pub poll_interval: Option<Duration>,
}

pub struct FolderEngine {
    pub record: FolderRecord,
    pub handle: Arc<EngineHandle>,
    pub folder_id_bytes: [u8; 16],
    pub restart_count: u32,
    identity: DeviceIdentity,
    transport: Arc<dyn ferry_sync::Transport>,
    options: EngineSpawnOptions,
    broadcast_tx: tokio::sync::broadcast::Sender<UiEvent>,
    watcher_shutdown_tx: Option<tokio::sync::watch::Sender<bool>>,
    watcher_task: Option<tokio::task::JoinHandle<()>>,
}

impl FolderEngine {
    pub fn start(
        folder_path: &Path,
        identity: &DeviceIdentity,
        transport: Arc<dyn ferry_sync::Transport>,
        options: EngineSpawnOptions,
        broadcast_tx: tokio::sync::broadcast::Sender<UiEvent>,
    ) -> Result<Self, SupervisorError> {
        Self::start_internal(
            folder_path,
            identity,
            transport,
            options,
            broadcast_tx,
            None,
            0,
        )
    }

    pub fn start_with_record(
        record: FolderRecord,
        identity: &DeviceIdentity,
        transport: Arc<dyn ferry_sync::Transport>,
        options: EngineSpawnOptions,
        broadcast_tx: tokio::sync::broadcast::Sender<UiEvent>,
    ) -> Result<Self, SupervisorError> {
        let path = record.path.clone();
        Self::start_internal(
            &path,
            identity,
            transport,
            options,
            broadcast_tx,
            Some(record),
            0,
        )
    }

    fn start_internal(
        folder_path: &Path,
        identity: &DeviceIdentity,
        transport: Arc<dyn ferry_sync::Transport>,
        options: EngineSpawnOptions,
        broadcast_tx: tokio::sync::broadcast::Sender<UiEvent>,
        existing_record: Option<FolderRecord>,
        restart_count: u32,
    ) -> Result<Self, SupervisorError> {
        let opened = ferry_folder::folder::open_folder(folder_path, identity).map_err(|e| {
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
            bind_addr: options.bind_addr,
            connect_to: options.connect_to,
            allow_trust_on_first_use: false,
            pin_state_dir: Some(opened.state_dir()),
            quiet: true,
        };
        let mut engine =
            SyncEngine::with_store(cfg, Arc::clone(&transport), Arc::clone(&opened.store))
                .map_err(|e| SupervisorError {
                    code: "bind".to_string(),
                    message: e.to_string(),
                })?;
        engine.set_identity(identity.clone());
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

        let (watcher_shutdown_tx, watcher_task) = spawn_engine_watcher(
            Arc::clone(&handle),
            record.path.join(".ferry"),
            broadcast_tx.clone(),
        );

        Ok(Self {
            record,
            handle,
            folder_id_bytes: opened.folder_id,
            restart_count,
            identity: identity.clone(),
            transport,
            options,
            broadcast_tx,
            watcher_shutdown_tx,
            watcher_task,
        })
    }

    /// Supervision tick: detect unhealthy engine loops and recover with exponential backoff.
    pub fn tick(&mut self) -> bool {
        if self.watcher_task.is_none() {
            let (tx, task) = spawn_engine_watcher(
                Arc::clone(&self.handle),
                self.record.path.join(".ferry"),
                self.broadcast_tx.clone(),
            );
            self.watcher_shutdown_tx = tx;
            self.watcher_task = task;
        }

        if !self.handle.is_healthy() {
            let backoff_ms = 100u64.saturating_mul(1u64 << self.restart_count.min(5));
            self.stop_watcher();
            self.handle.shutdown();

            let _ = self.broadcast_tx.send(UiEvent::Error {
                code: "engine-crashed".to_string(),
                message: format!(
                    "engine {} crashed, restarting with {backoff_ms}ms backoff",
                    self.record.folder_id
                ),
            });

            match Self::start_internal(
                &self.record.path,
                &self.identity,
                Arc::clone(&self.transport),
                self.options.clone(),
                self.broadcast_tx.clone(),
                Some(self.record.clone()),
                self.restart_count.saturating_add(1),
            ) {
                Ok(new_engine) => {
                    *self = new_engine;
                    true
                }
                Err(e) => {
                    let _ = self.broadcast_tx.send(UiEvent::Error {
                        code: e.code,
                        message: e.message,
                    });
                    false
                }
            }
        } else {
            false
        }
    }

    fn stop_watcher(&mut self) {
        if let Some(tx) = self.watcher_shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(task) = self.watcher_task.take() {
            task.abort();
        }
    }

    pub fn shutdown(&mut self) {
        self.stop_watcher();
        self.handle.shutdown();
    }

    pub fn snapshot(&self) -> EngineSnapshot {
        let manifest_id = self.handle.current_manifest_id().map(|id| hex_str(&id));
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
        let state = if manifest_id.is_some() {
            "idle".to_string()
        } else {
            "initializing".to_string()
        };
        let mut snap = EngineSnapshot::new(
            self.record.path.display().to_string(),
            self.record.folder_id.clone(),
            String::new(),
            state,
        );
        snap.manifest_id = manifest_id;
        snap.scanned = scanned;
        snap.pending_changes = self.handle.pending_changes();
        snap
    }

    pub fn handle(&self) -> &Arc<EngineHandle> {
        &self.handle
    }

    pub fn record(&self) -> &FolderRecord {
        &self.record
    }

    pub fn folder_id(&self) -> &str {
        &self.record.folder_id
    }

    pub fn folder_id_bytes(&self) -> [u8; 16] {
        self.folder_id_bytes
    }

    pub fn restart_count(&self) -> u32 {
        self.restart_count
    }

    pub fn is_healthy(&self) -> bool {
        self.handle.is_healthy()
    }

    pub fn broadcast_tx(&self) -> &tokio::sync::broadcast::Sender<UiEvent> {
        &self.broadcast_tx
    }
}

impl Drop for FolderEngine {
    fn drop(&mut self) {
        self.stop_watcher();
    }
}

fn spawn_engine_watcher(
    handle: Arc<EngineHandle>,
    state_dir: PathBuf,
    broadcast_tx: tokio::sync::broadcast::Sender<UiEvent>,
) -> (
    Option<tokio::sync::watch::Sender<bool>>,
    Option<tokio::task::JoinHandle<()>>,
) {
    if let Ok(rt_handle) = tokio::runtime::Handle::try_current() {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let task = rt_handle.spawn(async move {
            let mut last_manifest = None;
            let mut last_agreed = None;
            let mut last_scanned = None;
            let mut last_pending = None;

            let conflicts_file = state_dir.join("conflicts.jsonl");
            let mut last_meta = std::fs::metadata(&conflicts_file)
                .ok()
                .map(|m| (m.len(), m.modified().ok()));
            let mut last_conflicts_count = if last_meta.is_some() {
                ferry_sync_engine::list_conflicts(&state_dir).map_or(0, |c| c.len())
            } else {
                0
            };

            let mut interval = tokio::time::interval(Duration::from_millis(50));
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    _ = interval.tick() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }

                        let cur_manifest = handle.current_manifest_id();
                        let cur_agreed = handle.agreed_id();
                        let cur_scan = handle.scan_counts().map(|s| {
                            ScanStatsView::new(
                                s.files as u64,
                                s.dirs as u64,
                                s.symlinks as u64,
                                s.bytes_chunked,
                            )
                        });
                        let cur_pending = handle.pending_changes();

                        let changed = cur_manifest != last_manifest
                            || cur_agreed != last_agreed
                            || cur_scan != last_scanned
                            || cur_pending != last_pending;

                        if changed {
                            last_manifest = cur_manifest;
                            last_agreed = cur_agreed;
                            last_scanned = cur_scan;
                            last_pending = cur_pending;

                            let manifest_hex = cur_manifest.map(|r| hex_str(&r)).unwrap_or_default();
                            let agreed_hex = cur_agreed.map(|a| hex_str(&a));
                            let state_str = if cur_manifest.is_some() {
                                "idle".to_string()
                            } else {
                                "initializing".to_string()
                            };

                            let _ = broadcast_tx.send(UiEvent::StateChanged {
                                state: state_str,
                                manifest_id: manifest_hex,
                                agreed_id: agreed_hex,
                                pending_changes: cur_pending,
                                stats: cur_scan,
                            });
                        }

                        let cur_meta = std::fs::metadata(&conflicts_file)
                            .ok()
                            .map(|m| (m.len(), m.modified().ok()));
                        if cur_meta != last_meta {
                            last_meta = cur_meta;
                            if let Ok(conflicts) = ferry_sync_engine::list_conflicts(&state_dir) {
                                if conflicts.len() > last_conflicts_count {
                                    for entry in &conflicts[last_conflicts_count..] {
                                        let ts = ferry_platform::time::parse_rfc3339_to_unix(&entry.ts)
                                            .unwrap_or_else(|| ferry_platform::time::now_unix().0 as u64);
                                        let _ = broadcast_tx.send(UiEvent::ConflictRecorded {
                                            path: entry.path.clone(),
                                            conflict_path: entry
                                                .quarantined_as
                                                .clone()
                                                .unwrap_or_else(|| entry.path.clone()),
                                            timestamp: ts,
                                            quarantined_as: entry.quarantined_as.clone(),
                                        });
                                    }
                                    last_conflicts_count = conflicts.len();
                                } else if conflicts.len() < last_conflicts_count {
                                    last_conflicts_count = conflicts.len();
                                }
                            }
                        }
                    }
                }
            }
        });
        (Some(shutdown_tx), Some(task))
    } else {
        (None, None)
    }
}
