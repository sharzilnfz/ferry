use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use ferry_ipc::error::IpcError;
use ferry_ipc::framing::IpcConnection;
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, ScanStatsView};
use ferry_ipc::IpcServer;
use ferry_store::format::hex as hex_str;

use crate::state::DaemonState;

pub struct IpcServerHandle {
    socket_path: PathBuf,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    server_task: Option<tokio::task::JoinHandle<()>>,
    watcher_task: Option<tokio::task::JoinHandle<()>>,
    _runtime: Option<tokio::runtime::Runtime>,
}

impl IpcServerHandle {
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.server_task.take() {
            task.abort();
        }
        if let Some(task) = self.watcher_task.take() {
            task.abort();
        }
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

impl Drop for IpcServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(task) = self.server_task.take() {
            task.abort();
        }
        if let Some(task) = self.watcher_task.take() {
            task.abort();
        }
        #[cfg(unix)]
        {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

pub fn spawn_ipc_server(
    socket_path: PathBuf,
    state: Arc<DaemonState>,
) -> Result<IpcServerHandle, IpcError> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    let (server_task, watcher_task, runtime) =
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let server = IpcServer::bind(&socket_path)?;
            let st_clone = Arc::clone(&state);
            let s_rx = shutdown_rx.clone();
            let server_task = handle.spawn(async move {
                run_server_loop(server, st_clone, s_rx).await;
            });

            let w_rx = shutdown_rx;
            let watcher_task = handle.spawn(async move {
                run_state_watcher(state, w_rx).await;
            });

            (Some(server_task), Some(watcher_task), None)
        } else {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(IpcError::Io)?;

            let server = {
                let _guard = rt.enter();
                IpcServer::bind(&socket_path)?
            };

            let st_clone = Arc::clone(&state);
            let s_rx = shutdown_rx.clone();
            let server_task = rt.spawn(async move {
                run_server_loop(server, st_clone, s_rx).await;
            });

            let w_rx = shutdown_rx;
            let watcher_task = rt.spawn(async move {
                run_state_watcher(state, w_rx).await;
            });

            (Some(server_task), Some(watcher_task), Some(rt))
        };

    Ok(IpcServerHandle {
        socket_path,
        shutdown_tx,
        server_task,
        watcher_task,
        _runtime: runtime,
    })
}

async fn run_server_loop(
    server: IpcServer,
    state: Arc<DaemonState>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            accept_res = server.accept() => {
                match accept_res {
                    Ok(conn) => {
                        let st = Arc::clone(&state);
                        tokio::spawn(async move {
                            handle_client_connection(conn, st).await;
                        });
                    }
                    Err(e) => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                        eprintln!("[ferry-ipc] accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    server.close();
}

pub async fn handle_client_connection<S>(mut conn: IpcConnection<S>, state: Arc<DaemonState>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let initial_snapshot = state.snapshot();
    if let Err(e) = conn
        .send_message(&DaemonMessage::Snapshot(initial_snapshot))
        .await
    {
        eprintln!("[ferry-ipc] failed to send initial snapshot: {e}");
        return;
    }

    let (mut sender, mut receiver) = conn.split();
    let mut broadcast_rx = state.broadcast_tx().subscribe();

    loop {
        tokio::select! {
            cmd_res = receiver.recv_command() => {
                match cmd_res {
                    Ok(Some(cmd)) => {
                        let response = dispatch_client_command(&state, cmd);
                        if sender.send_message(&response).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) => {

                        break;
                    }
                    Err(e) => {
                        let err_msg = DaemonMessage::Error {
                            code: "bad_command".to_string(),
                            message: e.to_string(),
                        };
                        let _ = sender.send_message(&err_msg).await;
                        break;
                    }
                }
            }
            msg_res = broadcast_rx.recv() => {
                match msg_res {
                    Ok(msg) => {
                        if sender.send_message(&msg).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {

                        let snap = state.snapshot();
                        if sender.send_message(&DaemonMessage::Snapshot(snap)).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        }
    }
}

use ferry_folder::inventory::{ferry_home as ferry_home_for_registry, FolderInventory};

fn registry_error(e: ferry_folder::FolderError) -> DaemonMessage {
    DaemonMessage::Error {
        code: e.code.to_string(),
        message: e.message,
    }
}

fn pairing_ritual(
    home: PathBuf,
    identity: ferry_crypto::identity::DeviceIdentity,
) -> ferry_folder::pairing::PairingRitual {
    ferry_folder::pairing::PairingRitual::with_shared(
        home,
        identity,
        ferry_folder::pairing::shared_rendezvous(),
    )
}

fn expires_rfc3339(t: std::time::SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_else(|_| {
        std::time::Duration::from_secs(ferry_platform::time::now_unix().0 as u64)
    });
    ferry_platform::time::fmt_rfc3339(secs.as_secs() as i64)
}

fn pin_error_to_message(e: ferry_sync_engine::pin::PinError) -> DaemonMessage {
    let code = match &e {
        ferry_sync_engine::pin::PinError::PinActive { .. } => "pin-active",
        ferry_sync_engine::pin::PinError::BadPattern { .. } => "bad-pattern",
        ferry_sync_engine::pin::PinError::Corrupt { .. } => "pin-state-corrupt",
        ferry_sync_engine::pin::PinError::LedgerCorrupt { .. } => "held-ledger-corrupt",
        ferry_sync_engine::pin::PinError::ManifestMissing { .. } => "held-manifest-missing",
        ferry_sync_engine::pin::PinError::StructuralSplit { .. } => "structural-split",
        ferry_sync_engine::pin::PinError::Converge(_) => "pin-release-reconcile",
        _ => "pin_error",
    };
    DaemonMessage::Error {
        code: code.to_string(),
        message: e.to_string(),
    }
}

pub fn dispatch_common_command(
    home: &Path,
    identity: &ferry_crypto::identity::DeviceIdentity,
    cmd: &ClientCommand,
) -> Option<DaemonMessage> {
    match cmd {
        ClientCommand::Ping => Some(DaemonMessage::Pong),
        ClientCommand::ListDirectory { path } => {
            let inv = FolderInventory::new(home);
            Some(match inv.inspect_dir(path.clone()) {
                Ok(resp) => DaemonMessage::DirectoryListing {
                    entries: resp.entries,
                    absolute_path: resp.absolute_path,
                },
                Err(e) => registry_error(e),
            })
        }
        ClientCommand::CreatePairingSession { req } => {
            let ritual = pairing_ritual(home.to_path_buf(), identity.clone());
            Some(match ritual.create_offer_for_folder(&req.folder_id) {
                Ok(pending) => DaemonMessage::PairingCreated {
                    response: ferry_ipc::pairing::CreatePairingResponse::new(
                        pending.short_code,
                        expires_rfc3339(pending.expires_at),
                    ),
                },
                Err(e) => DaemonMessage::Error {
                    code: e.code.to_string(),
                    message: e.message,
                },
            })
        }
        ClientCommand::JoinPairingSession { req } => {
            let ritual = pairing_ritual(home.to_path_buf(), identity.clone());
            Some(
                match ritual
                    .accept_offer(&req.code, Some(&req.target_dir))
                    .and_then(|pending| pending.complete(0))
                {
                    Ok(accepted) => DaemonMessage::PairingJoined {
                        result: ferry_ipc::backend::PairResult {
                            folder_id: ferry_store::format::hex(&accepted.folder_id),
                            device_id: ferry_store::format::hex(identity.public()),
                            folder_path: accepted.folder,
                            status: "paired".to_string(),
                            message: Some("pairing completed over in-band transport".to_string()),
                        },
                    },
                    Err(e) => DaemonMessage::Error {
                        code: e.code.to_string(),
                        message: e.message,
                    },
                },
            )
        }
        _ => None,
    }
}

pub fn dispatch_client_command(state: &DaemonState, cmd: ClientCommand) -> DaemonMessage {
    if let Some(resp) = dispatch_common_command(&ferry_home_for_registry(), state.identity(), &cmd)
    {
        return resp;
    }
    match cmd {
        ClientCommand::GetStatus => DaemonMessage::Snapshot(state.snapshot()),
        ClientCommand::StartPin {
            paths,
            duration_hours,
        } => match state.start_pin(paths, duration_hours) {
            Ok(rec) => {
                let snap = state.snapshot();
                state.broadcast(DaemonMessage::StateChanged {
                    state: snap.state.clone(),
                    manifest_id: snap.manifest_id.unwrap_or_default(),
                    agreed_id: None,
                    pending_changes: snap.pending_changes,
                    stats: Some(snap.scanned),
                });
                DaemonMessage::Ack {
                    command: "start_pin".to_string(),
                    message: Some(format!("pinned {} path(s)", rec.paths.len())),
                }
            }
            Err(e) => pin_error_to_message(e),
        },
        ClientCommand::ReleasePin => match state.release_pin() {
            Ok(was_active) => {
                let snap = state.snapshot();
                state.broadcast(DaemonMessage::StateChanged {
                    state: snap.state.clone(),
                    manifest_id: snap.manifest_id.unwrap_or_default(),
                    agreed_id: None,
                    pending_changes: snap.pending_changes,
                    stats: Some(snap.scanned),
                });
                DaemonMessage::Ack {
                    command: "release_pin".to_string(),
                    message: Some(
                        if was_active {
                            "pin released"
                        } else {
                            "no active pin"
                        }
                        .to_string(),
                    ),
                }
            }
            Err(e) => DaemonMessage::Error {
                code: "pin_error".to_string(),
                message: e.to_string(),
            },
        },
        ClientCommand::TriggerScan => {
            state.trigger_scan();
            DaemonMessage::Ack {
                command: "trigger_scan".to_string(),
                message: Some("scan triggered".to_string()),
            }
        }
        ClientCommand::ListConflicts => match state.list_conflicts() {
            Ok(conflicts) => DaemonMessage::Ack {
                command: "list_conflicts".to_string(),
                message: Some(
                    serde_json::to_string(&conflicts).unwrap_or_else(|_| "[]".to_string()),
                ),
            },
            Err(e) => DaemonMessage::Error {
                code: "conflict_log".to_string(),
                message: e.to_string(),
            },
        },
        ClientCommand::ListFolders => {
            let inv = FolderInventory::new(&ferry_home_for_registry());
            match inv.list() {
                Ok(folders) => DaemonMessage::FolderList { folders },
                Err(e) => registry_error(e),
            }
        }
        ClientCommand::RegisterFolder { path } => {
            let inv = FolderInventory::new(&ferry_home_for_registry());
            match inv.register(&path) {
                Ok(rec) => DaemonMessage::FolderRegistered { folder: rec },
                Err(e) => registry_error(e),
            }
        }
        ClientCommand::RemoveFolder { folder_id } => {
            let inv = FolderInventory::new(&ferry_home_for_registry());
            match inv.unregister(&folder_id) {
                Ok(()) => DaemonMessage::FolderRemoved { folder_id },
                Err(e) => registry_error(e),
            }
        }
        ClientCommand::Ping
        | ClientCommand::ListDirectory { .. }
        | ClientCommand::CreatePairingSession { .. }
        | ClientCommand::JoinPairingSession { .. } => unreachable!(),
    }
}

pub fn dispatch_supervisor_command(
    supervisor: &mut crate::supervisor::Supervisor,
    cmd: ClientCommand,
) -> DaemonMessage {
    if let Some(resp) = dispatch_common_command(supervisor.home(), supervisor.identity(), &cmd) {
        return resp;
    }
    match cmd {
        ClientCommand::ListFolders => DaemonMessage::FolderList {
            folders: supervisor.list_folders(),
        },
        ClientCommand::RegisterFolder { path } => match supervisor.handle_register(path) {
            Ok(rec) => DaemonMessage::FolderRegistered { folder: rec },
            Err(e) => DaemonMessage::Error {
                code: e.code,
                message: e.message,
            },
        },
        ClientCommand::RemoveFolder { folder_id } => match supervisor.handle_remove(&folder_id) {
            Ok(()) => DaemonMessage::FolderRemoved { folder_id },
            Err(e) => DaemonMessage::Error {
                code: e.code,
                message: e.message,
            },
        },
        ClientCommand::GetStatus => match supervisor.get_status(None) {
            Ok(snap) => DaemonMessage::Snapshot(snap),
            Err(e) => DaemonMessage::Error {
                code: e.code,
                message: e.message,
            },
        },
        ClientCommand::StartPin {
            paths,
            duration_hours,
        } => {
            let (state_dir, folder_id_bytes) = match resolve_pin_state_dir(supervisor) {
                Ok(v) => v,
                Err(e) => return *e,
            };
            let mut base_agreements = std::collections::BTreeMap::new();
            if let Some(fid) = folder_id_bytes {
                if let Ok(ledger) =
                    ferry_store::agreement::AgreementLedger::new(&state_dir).list_folder(&fid)
                {
                    for (dev, rec) in ledger {
                        base_agreements.insert(hex_str(&dev), hex_str(&rec.manifest_id));
                    }
                }
            }
            let duration_secs = duration_hours.map(|h| h * 3600);
            match ferry_sync_engine::pin::PinManager::new(&state_dir).start_session_with_duration(
                paths.clone(),
                std::process::id(),
                &hex_str(supervisor.identity().public()),
                base_agreements,
                duration_secs,
            ) {
                Ok(rec) => DaemonMessage::Ack {
                    command: "start_pin".to_string(),
                    message: Some(format!("pinned {} path(s)", rec.paths.len())),
                },
                Err(e) => pin_error_to_message(e),
            }
        }
        ClientCommand::ReleasePin => {
            let state_dir = match resolve_pin_state_dir(supervisor) {
                Ok((d, _)) => {
                    if let Some(engine) = supervisor.engines_map().values().next() {
                        engine.handle.trigger_scan();
                    }
                    d
                }
                Err(e) => return *e,
            };
            match ferry_sync_engine::pin::PinManager::new(&state_dir).stop_session() {
                Ok(was_active) => DaemonMessage::Ack {
                    command: "release_pin".to_string(),
                    message: Some(
                        if was_active {
                            "pin released"
                        } else {
                            "no active pin"
                        }
                        .to_string(),
                    ),
                },
                Err(e) => DaemonMessage::Error {
                    code: "pin_error".to_string(),
                    message: e.to_string(),
                },
            }
        }
        ClientCommand::ListConflicts => {
            let state_dir = match resolve_pin_state_dir(supervisor) {
                Ok((d, _)) => d,
                Err(e) => return *e,
            };
            match ferry_sync_engine::list_conflicts(&state_dir) {
                Ok(conflicts) => DaemonMessage::Ack {
                    command: "list_conflicts".to_string(),
                    message: Some(
                        serde_json::to_string(&conflicts).unwrap_or_else(|_| "[]".to_string()),
                    ),
                },
                Err(e) => DaemonMessage::Error {
                    code: "conflict_log".to_string(),
                    message: e.to_string(),
                },
            }
        }
        ClientCommand::TriggerScan => {
            for engine in supervisor.engines_map().values() {
                engine.handle.trigger_scan();
            }
            DaemonMessage::Ack {
                command: "trigger_scan".to_string(),
                message: Some("scan triggered".to_string()),
            }
        }
        ClientCommand::Ping
        | ClientCommand::ListDirectory { .. }
        | ClientCommand::CreatePairingSession { .. }
        | ClientCommand::JoinPairingSession { .. } => unreachable!(),
    }
}

// Resolve which `.ferry` state directory a supervisor-side pin/conflicts command
// should target. Prefer an active engine (knows its folder id, so the agreement
// ledger is queryable); fall back to the first inventory record (folder id
// unknown — release falls back to the held ledger at recovery time). When the
// supervisor has neither, surface an explicit error so the CLI can fall back to
// local pin instead of writing into a CWD-relative `.ferry` like the old code.
fn resolve_pin_state_dir(
    supervisor: &crate::supervisor::Supervisor,
) -> Result<(PathBuf, Option<[u8; 16]>), Box<DaemonMessage>> {
    if let Some(engine) = supervisor.engines_map().values().next() {
        return Ok((engine.record.path.join(".ferry"), Some(engine.folder_id_bytes)));
    }
    if let Some(rec) = supervisor.inventory().list().unwrap_or_default().first() {
        return Ok((rec.path.join(".ferry"), None));
    }
    Err(Box::new(DaemonMessage::Error {
        code: "folder-not-found".to_string(),
        message: "no registered folder found in supervisor".to_string(),
    }))
}

pub async fn handle_supervisor_connection<S>(
    mut conn: IpcConnection<S>,
    supervisor: std::sync::Arc<tokio::sync::Mutex<crate::supervisor::Supervisor>>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let initial = {
        let sup = supervisor.lock().await;
        match sup.get_status(None) {
            Ok(snap) => DaemonMessage::Snapshot(snap),
            Err(_) => DaemonMessage::Snapshot(ferry_ipc::protocol::EngineSnapshot::new(
                "", "", "", "idle",
            )),
        }
    };
    if let Err(e) = conn.send_message(&initial).await {
        eprintln!("[ferry-ipc] failed to send initial supervisor snapshot: {e}");
        return;
    }
    let (mut sender, mut receiver) = conn.split();
    loop {
        match receiver.recv_command().await {
            Ok(Some(cmd)) => {
                let mut sup = supervisor.lock().await;
                let resp = dispatch_supervisor_command(&mut sup, cmd);
                if sender.send_message(&resp).await.is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                let err_msg = DaemonMessage::Error {
                    code: "bad_command".to_string(),
                    message: e.to_string(),
                };
                let _ = sender.send_message(&err_msg).await;
                break;
            }
        }
    }
}

pub fn spawn_supervisor_ipc_server(
    socket_path: PathBuf,
    supervisor: std::sync::Arc<tokio::sync::Mutex<crate::supervisor::Supervisor>>,
) -> Result<IpcServerHandle, IpcError> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (server_task, runtime) = if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let server = IpcServer::bind(&socket_path)?;
        let s_rx = shutdown_rx.clone();
        let sup_clone = std::sync::Arc::clone(&supervisor);
        let server_task = handle.spawn(async move {
            run_supervisor_server_loop(server, sup_clone, s_rx).await;
        });
        (Some(server_task), None)
    } else {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(IpcError::Io)?;
        let server = {
            let _guard = rt.enter();
            IpcServer::bind(&socket_path)?
        };
        let s_rx = shutdown_rx.clone();
        let sup_clone = std::sync::Arc::clone(&supervisor);
        let server_task = rt.spawn(async move {
            run_supervisor_server_loop(server, sup_clone, s_rx).await;
        });
        (Some(server_task), Some(rt))
    };
    Ok(IpcServerHandle {
        socket_path,
        shutdown_tx,
        server_task,
        watcher_task: None,
        _runtime: runtime,
    })
}

async fn run_supervisor_server_loop(
    server: IpcServer,
    supervisor: std::sync::Arc<tokio::sync::Mutex<crate::supervisor::Supervisor>>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            accept_res = server.accept() => {
                match accept_res {
                    Ok(conn) => {
                        let sup = std::sync::Arc::clone(&supervisor);
                        tokio::spawn(async move {
                            handle_supervisor_connection(conn, sup).await;
                        });
                    }
                    Err(e) => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                        eprintln!("[ferry-ipc] accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
    server.close();
}

async fn run_state_watcher(
    state: Arc<DaemonState>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut last_manifest = state.handle().current_manifest_id();
    let mut last_agreed = state.handle().agreed_id();
    let mut last_scanned = state.handle().scan_counts().map(|s| {
        ScanStatsView::new(
            s.files as u64,
            s.dirs as u64,
            s.symlinks as u64,
            s.bytes_chunked,
        )
    });
    let mut last_pending = state.handle().pending_changes();
    let mut last_pin_holding = ferry_sync_engine::pin::PinManager::new(state.state_dir())
        .is_holding()
        .unwrap_or(false);

    let conflicts_file = state.state_dir().join("conflicts.jsonl");
    let mut last_meta = std::fs::metadata(&conflicts_file)
        .ok()
        .map(|m| (m.len(), m.modified().ok()));
    let mut last_conflicts_count = if last_meta.is_some() {
        state.list_conflicts().map_or(0, |c| c.len())
    } else {
        0
    };

    let mut interval = tokio::time::interval(Duration::from_millis(100));

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

                let cur_manifest = state.handle().current_manifest_id();
                let cur_agreed = state.handle().agreed_id();
                let cur_scan = state.handle().scan_counts().map(|s| {
                    ScanStatsView::new(s.files as u64, s.dirs as u64, s.symlinks as u64, s.bytes_chunked)
                });
                let cur_pending = state.handle().pending_changes();
                let cur_pin_holding = ferry_sync_engine::pin::PinManager::new(state.state_dir())
                    .is_holding()
                    .unwrap_or(false);

                let changed = cur_manifest != last_manifest
                    || cur_agreed != last_agreed
                    || cur_scan != last_scanned
                    || cur_pending != last_pending
                    || cur_pin_holding != last_pin_holding;

                if changed {
                    last_manifest = cur_manifest;
                    last_agreed = cur_agreed;
                    last_scanned = cur_scan;
                    last_pending = cur_pending;
                    last_pin_holding = cur_pin_holding;

                    let manifest_hex = cur_manifest.map(|r| hex_str(&r)).unwrap_or_default();
                    let agreed_hex = cur_agreed.map(|a| hex_str(&a));
                    let state_str = if cur_manifest.is_some() {
                        "idle".to_string()
                    } else {
                        "initializing".to_string()
                    };

                    state.broadcast(DaemonMessage::StateChanged {
                        state: state_str,
                        manifest_id: manifest_hex,
                        agreed_id: agreed_hex,
                        pending_changes: cur_pending,
                        stats: cur_scan,
                    });
                }


                let cur_meta = std::fs::metadata(&conflicts_file).ok().map(|m| (m.len(), m.modified().ok()));
                if cur_meta != last_meta {
                    last_meta = cur_meta;
                    if let Ok(conflicts) = state.list_conflicts() {
                        if conflicts.len() > last_conflicts_count {
                            for entry in &conflicts[last_conflicts_count..] {
                                let ts = ferry_platform::time::parse_rfc3339_to_unix(&entry.ts)
                                    .unwrap_or_else(|| ferry_platform::time::now_unix().0 as u64);
                                state.broadcast(DaemonMessage::ConflictRecorded {
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
}
