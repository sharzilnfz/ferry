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
use ferry_ipc::protocol::{
    ClientCommand, DaemonMessage, ScanStatsView,
};
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

    let (server_task, watcher_task, runtime) = if let Ok(handle) = tokio::runtime::Handle::try_current() {
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
pub async fn handle_client_connection<S>(
    mut conn: IpcConnection<S>,
    state: Arc<DaemonState>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // 1. Send initial snapshot immediately upon connection
    let initial_snapshot = state.snapshot();
    if let Err(e) = conn.send_message(&DaemonMessage::Snapshot(initial_snapshot)).await {
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

/// Process a single client command and return the immediate response message.
pub fn dispatch_client_command(state: &DaemonState, cmd: ClientCommand) -> DaemonMessage {
    match cmd {
        ClientCommand::GetStatus => {
            DaemonMessage::Snapshot(state.snapshot())
        }
        ClientCommand::StartPin { paths } => {
            match state.start_pin(paths) {
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
                Err(e) => DaemonMessage::Error {
                    code: "pin_error".to_string(),
                    message: e.to_string(),
                },
            }
        }
        ClientCommand::ReleasePin => {
            match state.release_pin() {
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
            }
        }
        ClientCommand::TriggerScan => {
            state.trigger_scan();
            DaemonMessage::Ack {
                command: "trigger_scan".to_string(),
                message: Some("scan triggered".to_string()),
            }
        }
        ClientCommand::ListConflicts => {
            match state.list_conflicts() {
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
        ClientCommand::Ping => DaemonMessage::Pong,
    }
}

/// Background watcher task that monitors engine state transitions and new conflict records.
async fn run_state_watcher(
    state: Arc<DaemonState>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut last_manifest = state.handle().root_id();
    let mut last_agreed = state.handle().agreed_id();
    let mut last_scanned = state
        .handle()
        .scan_counts()
        .map(|s| ScanStatsView::new(s.files as u64, s.dirs as u64, s.symlinks as u64, s.bytes_chunked));
    let mut last_pending = state.handle().pending_changes();
    let mut last_conflicts_count = state.list_conflicts().map_or(0, |c| c.len());

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

                let cur_root = state.handle().root_id();
                let cur_agreed = state.handle().agreed_id();
                let cur_scan = state.handle().scan_counts().map(|s| {
                    ScanStatsView::new(s.files as u64, s.dirs as u64, s.symlinks as u64, s.bytes_chunked)
                });
                let cur_pending = state.handle().pending_changes();

                let changed = cur_root != last_manifest
                    || cur_agreed != last_agreed
                    || cur_scan != last_scanned
                    || cur_pending != last_pending;

                if changed {
                    last_manifest = cur_root;
                    last_agreed = cur_agreed;
                    last_scanned = cur_scan;
                    last_pending = cur_pending;

                    let manifest_hex = cur_root.map(|r| hex_str(&r)).unwrap_or_default();
                    let agreed_hex = cur_agreed.map(|a| hex_str(&a));
                    let state_str = if cur_root.is_some() {
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

                // Check for new conflicts in .ferry/conflicts.jsonl
                if let Ok(conflicts) = state.list_conflicts() {
                    if conflicts.len() > last_conflicts_count {
                        for entry in &conflicts[last_conflicts_count..] {
                            let ts = crate::timefmt::parse_rfc3339_to_unix(&entry.ts)
                                .unwrap_or_else(|| crate::timefmt::now_unix().0 as u64);
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
