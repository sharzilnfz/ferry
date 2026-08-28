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
                        let response = dispatch_client_command(&state, cmd).await;
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
pub async fn dispatch_client_command(state: &DaemonState, cmd: ClientCommand) -> DaemonMessage {
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
        ClientCommand::ListDirectory { path } => {
            let p = path.map_or_else(
                || std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                std::path::PathBuf::from,
            );
            let mut entries = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&p) {
                for entry in rd.flatten() {
                    let ep = entry.path();
                    let is_dir = ep.is_dir();
                    let is_symlink = ep.is_symlink();
                    let is_git_repo = is_dir && ep.join(".git").exists();
                    let is_already_synced = is_dir && ep.join(".ferry").exists();
                    let name = entry.file_name().to_string_lossy().to_string();
                    entries.push(ferry_ipc::backend::FsEntry {
                        name,
                        path: ep,
                        is_dir,
                        is_symlink,
                        is_git_repo,
                        is_already_synced,
                    });
                }
            }
            let listing = ferry_ipc::backend::DirectoryListing {
                parent_path: p.parent().map(std::path::PathBuf::from),
                current_path: p,
                entries,
            };
            DaemonMessage::Ack {
                command: "list_directory".to_string(),
                message: serde_json::to_string(&listing).ok(),
            }
        }
        ClientCommand::ListFolders => {
            let folders = state.list_folders();
            DaemonMessage::Ack {
                command: "list_folders".to_string(),
                message: serde_json::to_string(&folders).ok(),
            }
        }
        ClientCommand::RegisterFolder { path } => match state.register_folder(PathBuf::from(path)) {
            Ok(info) => DaemonMessage::Ack {
                command: "register_folder".to_string(),
                message: serde_json::to_string(&info).ok(),
            },
            Err(e) => DaemonMessage::Error {
                code: e.code,
                message: e.message,
            },
        },
        ClientCommand::UnregisterFolder { folder_id } => {
            match state.unregister_folder(&folder_id) {
                Ok(()) => DaemonMessage::Ack {
                    command: "unregister_folder".to_string(),
                    message: Some(folder_id),
                },
                Err(e) => DaemonMessage::Error {
                    code: e.code,
                    message: e.message,
                },
            }
        }
        ClientCommand::SwitchFolder { folder_id } => match state.switch_folder(&folder_id) {
            Ok(snap) => DaemonMessage::Snapshot(snap),
            Err(e) => DaemonMessage::Error {
                code: e.code,
                message: e.message,
            },
        },
        ClientCommand::CreatePairingSession { folder_id } => {
            let fid_str = folder_id.unwrap_or_else(|| state.snapshot().folder_id);
            let (folder_path, sync_addr) = if let Some(fid_bytes) = ferry_store::format::unhex::<16>(&fid_str) {
                let p = state.store_dir_for(&fid_bytes).unwrap_or_else(|| state.tree_dir());
                let a = state.managed_folders().into_iter().find(|m| m.folder_id == fid_bytes).and_then(|m| m.listen_addr);
                (p, a)
            } else {
                (state.tree_dir(), None)
            };

            match crate::pairing::start_host_pairing(&folder_path, state.identity(), Some(fid_str), sync_addr) {
                Ok(host_sess) => DaemonMessage::Ack {
                    command: "create_pairing_session".to_string(),
                    message: serde_json::to_string(&host_sess.session).ok(),
                },
                Err(e) => DaemonMessage::Error {
                    code: e.code,
                    message: e.message,
                },
            }
        }
        ClientCommand::JoinPairingSession { code, target_dir } => {
            let p = target_dir
                .map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from);
            let abs_path = if p.is_relative() {
                std::env::current_dir().map_or_else(|_| p.clone(), |cwd| cwd.join(&p))
            } else {
                p.clone()
            };

            match crate::pairing::execute_joiner_pairing(&code, &abs_path, state.identity()).await {
                Ok(res) => {
                    let _ = state.register_folder(abs_path.clone());
                    DaemonMessage::Ack {
                        command: "join_pairing_session".to_string(),
                        message: serde_json::to_string(&res).ok(),
                    }
                }
                Err(e) => DaemonMessage::Error {
                    code: e.code,
                    message: e.message,
                },
            }
        }
        ClientCommand::Ping => DaemonMessage::Pong,
    }
}

struct FolderTrack {
    last_manifest: Option<[u8; 32]>,
    last_agreed: Option<[u8; 32]>,
    last_scanned: Option<ScanStatsView>,
    last_pending: Option<i64>,
    last_pin_holding: bool,
    last_meta: Option<(u64, Option<std::time::SystemTime>)>,
    last_conflicts_count: usize,
}

/// Background watcher task that monitors engine state transitions and new conflict records across all folders.
async fn run_state_watcher(
    state: Arc<DaemonState>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut tracked: std::collections::HashMap<[u8; 16], FolderTrack> = std::collections::HashMap::new();
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

                let managed = state.managed_folders();
                let active_id = state.active_folder_id();

                let current_ids: std::collections::HashSet<[u8; 16]> =
                    managed.iter().map(|m| m.folder_id).collect();
                tracked.retain(|fid, _| current_ids.contains(fid));

                for m in managed {
                    let fid = m.folder_id;
                    let state_dir = state
                        .state_dir_for(&fid)
                        .unwrap_or_else(|| m.store_dir.join(".ferry"));
                    let conflicts_file = state_dir.join("conflicts.jsonl");

                    let track = tracked.entry(fid).or_insert_with(|| {
                        let cur_meta = std::fs::metadata(&conflicts_file)
                            .ok()
                            .map(|meta| (meta.len(), meta.modified().ok()));
                        let cur_conflicts =
                            ferry_sync_engine::list_conflicts(&state_dir).map_or(0, |c| c.len());
                        FolderTrack {
                            last_manifest: m.handle.current_manifest_id(),
                            last_agreed: m.handle.agreed_id(),
                            last_scanned: m.handle.scan_counts().map(|s| {
                                ScanStatsView::new(
                                    s.files as u64,
                                    s.dirs as u64,
                                    s.symlinks as u64,
                                    s.bytes_chunked,
                                )
                            }),
                            last_pending: m.handle.pending_changes(),
                            last_pin_holding: ferry_pin::PinManager::new(&state_dir)
                                .is_holding()
                                .unwrap_or(false),
                            last_meta: cur_meta,
                            last_conflicts_count: cur_conflicts,
                        }
                    });

                    let cur_manifest = m.handle.current_manifest_id();
                    let cur_agreed = m.handle.agreed_id();
                    let cur_scan = m.handle.scan_counts().map(|s| {
                        ScanStatsView::new(
                            s.files as u64,
                            s.dirs as u64,
                            s.symlinks as u64,
                            s.bytes_chunked,
                        )
                    });
                    let cur_pending = m.handle.pending_changes();
                    let cur_pin_holding = ferry_pin::PinManager::new(&state_dir)
                        .is_holding()
                        .unwrap_or(false);

                    let changed = cur_manifest != track.last_manifest
                        || cur_agreed != track.last_agreed
                        || cur_scan != track.last_scanned
                        || cur_pending != track.last_pending
                        || cur_pin_holding != track.last_pin_holding;

                    if changed {
                        track.last_manifest = cur_manifest;
                        track.last_agreed = cur_agreed;
                        track.last_scanned = cur_scan;
                        track.last_pending = cur_pending;
                        track.last_pin_holding = cur_pin_holding;

                        if Some(fid) == active_id {
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
                    }

                    // Check for new conflicts in .ferry/conflicts.jsonl when file metadata changes
                    let cur_meta = std::fs::metadata(&conflicts_file)
                        .ok()
                        .map(|meta| (meta.len(), meta.modified().ok()));
                    if cur_meta != track.last_meta {
                        track.last_meta = cur_meta;
                        if let Ok(conflicts) = ferry_sync_engine::list_conflicts(&state_dir) {
                            if conflicts.len() > track.last_conflicts_count {
                                for entry in &conflicts[track.last_conflicts_count..] {
                                    let ts = ferry_platform::time::parse_rfc3339_to_unix(&entry.ts)
                                        .unwrap_or_else(|| {
                                            ferry_platform::time::now_unix().0 as u64
                                        });
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
                                track.last_conflicts_count = conflicts.len();
                            } else if conflicts.len() < track.last_conflicts_count {
                                track.last_conflicts_count = conflicts.len();
                            }
                        }
                    }
                }
            }
        }
    }
}
