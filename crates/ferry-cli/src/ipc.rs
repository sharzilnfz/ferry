use std::path::Path;
use std::time::Duration;

use ferry_ipc::paths::socket_path_for_dir;
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, EngineSnapshot};
use ferry_ipc::IpcClient;

pub const IPC_TIMEOUT: Duration = Duration::from_millis(500);

#[cfg(unix)]
type IpcConn = ferry_ipc::framing::IpcConnection<tokio::net::UnixStream>;
#[cfg(windows)]
type IpcConn = ferry_ipc::framing::IpcConnection<tokio::net::windows::named_pipe::NamedPipeClient>;

async fn connect_ipc(folder: &Path) -> Option<IpcConn> {
    // Probe whichever socket is actually present. Ticket 03 prefers the device
    // daemon for persistent services, but tests (and any leftover single-store
    // daemon) bind only to the per-folder socket — checking existence before
    // connect avoids the order dependency entirely.
    let folder_sock = socket_path_for_dir(folder);
    let default_sock = ferry_ipc::paths::default_socket_path();
    let (first, second) = if folder_sock.exists() {
        (&folder_sock, &default_sock)
    } else {
        (&default_sock, &folder_sock)
    };
    if let Ok(conn) = IpcClient::connect(first).await {
        return Some(conn);
    }
    if first != second {
        if let Ok(conn) = IpcClient::connect(second).await {
            return Some(conn);
        }
    }
    None
}

pub fn query_status(folder: &Path) -> Option<EngineSnapshot> {
    let folder_owned = folder.to_path_buf();
    run_async(IPC_TIMEOUT, async move {
        let mut conn = connect_ipc(&folder_owned).await?;

        match tokio::time::timeout(IPC_TIMEOUT, conn.recv_message())
            .await
            .ok()?
            .ok()?
        {
            Some(DaemonMessage::Snapshot(snap)) => Some(snap),
            _ => {
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

pub fn send_command(folder: &Path, cmd: ClientCommand) -> Option<DaemonMessage> {
    let folder_owned = folder.to_path_buf();
    run_async(IPC_TIMEOUT, async move {
        let mut conn = connect_ipc(&folder_owned).await?;

        let _ = tokio::time::timeout(Duration::from_millis(50), conn.recv_message()).await;

        conn.send_command(&cmd).await.ok()?;
        let resp = tokio::time::timeout(IPC_TIMEOUT, conn.recv_message())
            .await
            .ok()?
            .ok()?;
        resp
    })
}

pub fn query_conflicts(folder: &Path) -> Option<Vec<ferry_sync_engine::ConflictEntry>> {
    let folder_owned = folder.to_path_buf();
    run_async(IPC_TIMEOUT, async move {
        let mut conn = connect_ipc(&folder_owned).await?;

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

fn run_async<F, T>(timeout: Duration, f: F) -> Option<T>
where
    F: std::future::Future<Output = Option<T>> + Send + 'static,
    T: Send + 'static,
{
    // Always run on a fresh current-thread runtime so callers in any context
    // (sync main, multi-thread Tokio, current-thread Tokio) behave identically.
    // Wrap `f` in `tokio::time::timeout` *inside* `block_on` so a hanging
    // `connect_ipc` or `recv_message` is cancelled and the thread exits — the
    // outer `recv_timeout` is just the caller-side gate.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let res = rt.block_on(async { tokio::time::timeout(timeout, f).await.ok().flatten() });
        let _ = tx.send(res);
        Some(())
    });
    rx.recv_timeout(timeout).ok().flatten()
}
