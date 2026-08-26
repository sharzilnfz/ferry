//! IPC client query helpers for talking to a running Ferry daemon.
//!
//! Provides fast (50-100ms timeout) queries for cached snapshots, commands, and conflicts,
//! returning `None` immediately if the daemon is offline, socket is missing, or query times out.

use std::path::Path;
use std::time::Duration;

use ferry_ipc::paths::socket_path_for_dir;
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, EngineSnapshot};
use ferry_ipc::IpcClient;

/// Short timeout for local IPC socket operations (connect, query, command).
pub const IPC_TIMEOUT: Duration = Duration::from_millis(80);

/// Query the running daemon for an instant engine snapshot over IPC.
///
/// Returns `Some(EngineSnapshot)` if the daemon socket is alive and responds within the timeout,
/// or `None` if the daemon is offline, socket is missing, connection fails, or query times out.
pub fn query_status(folder: &Path) -> Option<EngineSnapshot> {
    let socket_path = socket_path_for_dir(folder);
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&socket_path).await.ok()?;

        // On connection, the daemon sends an initial Snapshot message immediately.
        match tokio::time::timeout(IPC_TIMEOUT, conn.recv_message())
            .await
            .ok()?
            .ok()?
        {
            Some(DaemonMessage::Snapshot(snap)) => Some(snap),
            _ => {
                // If initial was not a snapshot, explicitly send GetStatus
                conn.send_command(&ClientCommand::GetStatus).await.ok()?;
                match tokio::time::timeout(IPC_TIMEOUT, conn.recv_message())
                    .await
                    .ok()?
                    .ok()?
                {
                    Some(DaemonMessage::Snapshot(snap)) => Some(snap),
                    _ => None,
                }
            }
        }
    })
}

/// Send a typed command to the daemon over IPC and await the response.
///
/// Returns `Some(DaemonMessage)` if the daemon responded within the timeout, or `None` if offline.
pub fn send_command(folder: &Path, cmd: ClientCommand) -> Option<DaemonMessage> {
    let socket_path = socket_path_for_dir(folder);
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&socket_path).await.ok()?;

        // Drain initial snapshot if present (with a very short timeout)
        let _ = tokio::time::timeout(Duration::from_millis(20), conn.recv_message()).await;

        conn.send_command(&cmd).await.ok()?;
        let resp = tokio::time::timeout(IPC_TIMEOUT, conn.recv_message())
            .await
            .ok()?
            .ok()?;
        resp
    })
}

/// Query conflict list from the running daemon over IPC.
///
/// Returns `Some(Vec<ConflictEntry>)` if the daemon responded with conflict list, or `None` if offline.
pub fn query_conflicts(folder: &Path) -> Option<Vec<ferry_sync_engine::ConflictEntry>> {
    let socket_path = socket_path_for_dir(folder);
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&socket_path).await.ok()?;

        // Drain initial snapshot
        let _ = tokio::time::timeout(Duration::from_millis(20), conn.recv_message()).await;

        conn.send_command(&ClientCommand::ListConflicts)
            .await
            .ok()?;
        let resp = tokio::time::timeout(IPC_TIMEOUT, conn.recv_message())
            .await
            .ok()?
            .ok()??;
        match resp {
            DaemonMessage::Ack {
                message: Some(msg), ..
            } => serde_json::from_str(&msg).ok(),
            _ => None,
        }
    })
}

/// Execute an async future in a synchronous context with a timeout.
///
/// Works safely in environments with no runtime, single-threaded runtimes,
/// or multi-threaded runtimes.
fn run_async<F, T>(timeout: Duration, f: F) -> Option<T>
where
    F: std::future::Future<Output = Option<T>> + Send + 'static,
    T: Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return tokio::task::block_in_place(|| {
                handle.block_on(async { tokio::time::timeout(timeout, f).await.ok().flatten() })
            });
        }
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async { tokio::time::timeout(timeout, f).await.ok().flatten() })
        })
        .join()
        .ok()
        .flatten()
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async { tokio::time::timeout(timeout, f).await.ok().flatten() })
    }
}
