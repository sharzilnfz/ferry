//! IPC server implementation for the sync daemon.
//!
//! Exposes a typed newline-delimited JSON wire protocol over local platform transports
//! (Unix domain sockets on Unix, named pipes on Windows).
//!
//! Connected clients receive:
//! - An immediate initial [`DaemonMessage::Snapshot`] containing the full folder and engine state.
//! - Live broadcasts of [`DaemonMessage::StateChanged`], [`DaemonMessage::TransferProgress`],
//!   and [`DaemonMessage::ConflictRecorded`].
//! - Responses to [`ClientCommand`] requests (`GetStatus`, `StartPin`, `ReleasePin`, `TriggerScan`, `ListConflicts`, `Ping`).

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

/// Handle to a running daemon IPC server.
pub struct IpcServerHandle {
    socket_path: PathBuf,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    server_task: Option<tokio::task::JoinHandle<()>>,
    watcher_task: Option<tokio::task::JoinHandle<()>>,
    _runtime: Option<tokio::runtime::Runtime>,
}

impl IpcServerHandle {
    /// Return the bound socket or pipe path.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Gracefully shutdown the IPC server and remove socket files.
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

/// Spawns the daemon IPC server listening at `socket_path`.
///
/// Supports execution inside or outside an active Tokio runtime.
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

/// Main accept loop for incoming IPC client connections.
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

/// Handle a single connected client over an arbitrary async duplex stream.
pub async fn handle_client_connection<S>(mut conn: IpcConnection<S>, state: Arc<DaemonState>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // 1. Send initial snapshot immediately upon connection
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
                        // EOF / client disconnected
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
                        // Resync with full snapshot
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

fn ferry_home_for_registry() -> std::path::PathBuf {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        if !home.as_os_str().is_empty() {
            return home.join(".ferry");
        }
    }
    PathBuf::from("/tmp/.ferry")
}

/// Process a single client command and return the immediate response message.
pub fn dispatch_client_command(state: &DaemonState, cmd: ClientCommand) -> DaemonMessage {
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
            Err(e) => {
                let code = match &e {
                    ferry_pin::PinError::PinActive { .. } => "pin-active",
                    ferry_pin::PinError::BadPattern { .. } => "bad-pattern",
                    ferry_pin::PinError::Corrupt { .. } => "pin-state-corrupt",
                    ferry_pin::PinError::LedgerCorrupt { .. } => "held-ledger-corrupt",
                    ferry_pin::PinError::ManifestMissing { .. } => "held-manifest-missing",
                    ferry_pin::PinError::StructuralSplit { .. } => "structural-split",
                    ferry_pin::PinError::Reconcile(_) => "pin-release-reconcile",
                    _ => "pin_error",
                };
                DaemonMessage::Error {
                    code: code.to_string(),
                    message: e.to_string(),
                }
            }
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
        ClientCommand::Ping => DaemonMessage::Pong,
        ClientCommand::ListDirectory { path } => {
            use ferry_ipc::fs::{list_directory_sync, validate_path};
            match validate_path(path) {
                Ok(validated) => match list_directory_sync(validated) {
                    Ok(resp) => DaemonMessage::DirectoryListing {
                        entries: resp.entries,
                        absolute_path: resp.absolute_path,
                    },
                    Err(e) => DaemonMessage::Error {
                        code: e.code,
                        message: e.message,
                    },
                },
                Err(e) => DaemonMessage::Error {
                    code: e.code,
                    message: e.message,
                },
            }
        }
        ClientCommand::ListFolders => {
            let home = ferry_home_for_registry();
            match crate::registry::FolderRegistry::load(&home) {
                Ok(reg) => DaemonMessage::FolderList {
                    folders: reg.folders,
                },
                Err(e) => DaemonMessage::Error {
                    code: e.code.clone(),
                    message: e.message.clone(),
                },
            }
        }
        ClientCommand::RegisterFolder { path } => {
            let home = ferry_home_for_registry();
            let mut reg = match crate::registry::FolderRegistry::load(&home) {
                Ok(r) => r,
                Err(e) => {
                    return DaemonMessage::Error {
                        code: e.code,
                        message: e.message,
                    }
                }
            };
            match reg.register(path) {
                Ok(rec) => {
                    if let Err(e) = reg.save(&home) {
                        return DaemonMessage::Error {
                            code: e.code,
                            message: e.message,
                        };
                    }
                    DaemonMessage::FolderRegistered { folder: rec }
                }
                Err(e) => DaemonMessage::Error {
                    code: e.code,
                    message: e.message,
                },
            }
        }
        ClientCommand::RemoveFolder { folder_id } => {
            let home = ferry_home_for_registry();
            let mut reg = match crate::registry::FolderRegistry::load(&home) {
                Ok(r) => r,
                Err(e) => {
                    return DaemonMessage::Error {
                        code: e.code,
                        message: e.message,
                    }
                }
            };
            match reg.remove(&folder_id) {
                Ok(()) => {
                    if let Err(e) = reg.save(&home) {
                        return DaemonMessage::Error {
                            code: e.code,
                            message: e.message,
                        };
                    }
                    DaemonMessage::FolderRemoved { folder_id }
                }
                Err(e) => DaemonMessage::Error {
                    code: e.code,
                    message: e.message,
                },
            }
        }
        ClientCommand::CreatePairingSession { req: _ } => DaemonMessage::Error {
            code: "not-implemented".to_string(),
            message: "create_pairing_session not implemented in this wave".to_string(),
        },
        ClientCommand::JoinPairingSession { req: _ } => DaemonMessage::Error {
            code: "not-implemented".to_string(),
            message: "join_pairing_session not implemented in this wave".to_string(),
        },
    }
}

/// Dispatch for the centralized Supervisor (multi-engine) daemon.
pub fn dispatch_supervisor_command(
    supervisor: &mut crate::supervisor::Supervisor,
    cmd: ClientCommand,
) -> DaemonMessage {
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
        ClientCommand::Ping => DaemonMessage::Pong,
        ClientCommand::ListDirectory { path } => {
            use ferry_ipc::fs::{list_directory_sync, validate_path};
            match validate_path(path) {
                Ok(validated) => match list_directory_sync(validated) {
                    Ok(resp) => DaemonMessage::DirectoryListing {
                        entries: resp.entries,
                        absolute_path: resp.absolute_path,
                    },
                    Err(e) => DaemonMessage::Error {
                        code: e.code,
                        message: e.message,
                    },
                },
                Err(e) => DaemonMessage::Error {
                    code: e.code,
                    message: e.message,
                },
            }
        }
        other => dispatch_client_command_fallback(other),
    }
}

fn dispatch_client_command_fallback(cmd: ClientCommand) -> DaemonMessage {
    match cmd {
        ClientCommand::CreatePairingSession { .. }
        | ClientCommand::JoinPairingSession { .. }
        | ClientCommand::StartPin { .. }
        | ClientCommand::ReleasePin
        | ClientCommand::TriggerScan
        | ClientCommand::ListConflicts => DaemonMessage::Error {
            code: "not-implemented".to_string(),
            message: "single-folder command not supported in supervisor context".to_string(),
        },
        _ => DaemonMessage::Error {
            code: "not-implemented".to_string(),
            message: "unsupported command".to_string(),
        },
    }
}

/// Handle a supervisor-backed client connection over a duplex stream.
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
                "",
                "",
                "",
                "idle",
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

/// Spawn a supervisor-backed IPC server.
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

/// Background watcher task that monitors engine state transitions and new conflict records.
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
    let mut last_pin_holding = ferry_pin::PinManager::new(state.state_dir())
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
                let cur_pin_holding = ferry_pin::PinManager::new(state.state_dir())
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

                // Check for new conflicts in .ferry/conflicts.jsonl when file metadata changes
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
