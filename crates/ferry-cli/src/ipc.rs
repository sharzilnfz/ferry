use std::path::Path;
use std::time::Duration;

use ferry_ipc::paths::socket_path_for_dir;
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, EngineSnapshot};
use ferry_ipc::IpcClient;

pub const IPC_TIMEOUT: Duration = Duration::from_millis(500);

pub fn query_status(folder: &Path) -> Option<EngineSnapshot> {
    let socket_path = socket_path_for_dir(folder);
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&socket_path).await.ok()?;

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
    let socket_path = socket_path_for_dir(folder);
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&socket_path).await.ok()?;

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
    let socket_path = socket_path_for_dir(folder);
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&socket_path).await.ok()?;

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
