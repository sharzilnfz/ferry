use std::path::Path;
use serde_json::{json, Value};
use ferry_ipc::error::IpcError;
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, EngineSnapshot};

/// Connect to daemon over IPC and retrieve the latest engine snapshot.
pub async fn query_daemon_status(socket_path: &Path) -> Result<EngineSnapshot, IpcError> {
    #[cfg(unix)]
    let mut conn = ferry_ipc::transport::unix::IpcClient::connect(socket_path).await?;
    #[cfg(windows)]
    let mut conn = ferry_ipc::transport::windows::IpcClient::connect(socket_path).await?;

    let initial = conn.recv_message().await?;
    match initial {
        Some(DaemonMessage::Snapshot(snap)) => Ok(snap),
        _ => {
            conn.send_command(&ClientCommand::GetStatus).await?;
            let resp = conn.recv_message().await?.ok_or_else(|| {
                IpcError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon disconnected",
                ))
            })?;
            match resp {
                DaemonMessage::Snapshot(snap) => Ok(snap),
                DaemonMessage::Error { code, message } => {
                    Err(IpcError::Protocol(format!("{code}: {message}")))
                }
                other => Err(IpcError::Protocol(format!("unexpected response: {other:?}"))),
            }
        }
    }
}

/// Connect to daemon over IPC and retrieve conflict list.
pub async fn query_daemon_conflicts(socket_path: &Path) -> Result<Vec<Value>, IpcError> {
    #[cfg(unix)]
    let mut conn = ferry_ipc::transport::unix::IpcClient::connect(socket_path).await?;
    #[cfg(windows)]
    let mut conn = ferry_ipc::transport::windows::IpcClient::connect(socket_path).await?;

    let _initial = conn.recv_message().await?;
    conn.send_command(&ClientCommand::ListConflicts).await?;
    let resp = conn.recv_message().await?.ok_or_else(|| {
        IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "daemon disconnected",
        ))
    })?;
    match resp {
        DaemonMessage::Ack {
            message: Some(json_str),
            ..
        } => {
            let entries: Vec<Value> = serde_json::from_str(&json_str).unwrap_or_default();
            Ok(entries)
        }
        DaemonMessage::Ack { message: None, .. } => Ok(Vec::new()),
        DaemonMessage::Error { code, message } => {
            Err(IpcError::Protocol(format!("{code}: {message}")))
        }
        other => Err(IpcError::Protocol(format!("unexpected response: {other:?}"))),
    }
}

/// Send a general command to the daemon over IPC and get the response message.
pub async fn query_daemon_command(
    socket_path: &Path,
    cmd: ClientCommand,
) -> Result<DaemonMessage, IpcError> {
    #[cfg(unix)]
    let mut conn = ferry_ipc::transport::unix::IpcClient::connect(socket_path).await?;
    #[cfg(windows)]
    let mut conn = ferry_ipc::transport::windows::IpcClient::connect(socket_path).await?;

    let _initial = conn.recv_message().await?;
    conn.send_command(&cmd).await?;
    let resp = conn.recv_message().await?.ok_or_else(|| {
        IpcError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "daemon disconnected",
        ))
    })?;
    Ok(resp)
}

/// Format an `EngineSnapshot` into the standard `status` JSON document shape.
#[must_use]
pub fn snapshot_to_status_doc(snap: &EngineSnapshot) -> Value {
    let mut held_by_peer_val = serde_json::Map::new();
    for (peer, paths) in &snap.held_by_peer {
        held_by_peer_val.insert(peer.clone(), json!(paths));
    }

    json!({
        "command": "status",
        "folder": snap.folder,
        "folder_id": snap.folder_id,
        "device_id": snap.device_id,
        "manifest_id": snap.manifest_id.clone().unwrap_or_default(),
        "scanned": {
            "files": snap.scanned.files,
            "dirs": snap.scanned.dirs,
            "symlinks": snap.scanned.symlinks,
            "bytes_chunked": snap.scanned.bytes_chunked,
        },
        "pending_changes": snap.pending_changes,
        "pin": {
            "state": snap.pin.state,
            "holding": snap.pin.holding,
            "paths": snap.pin.paths,
        },
        "held_changes": snap.held_changes,
        "held_by_peer": Value::Object(held_by_peer_val),
        "peers": snap.peers.iter().map(|p| json!({
            "device_id": p.device_id,
            "last_agreed_manifest_id": p.last_agreed_manifest_id,
            "agreed_at": p.agreed_at,
            "connectivity": p.connectivity,
        })).collect::<Vec<_>>(),
        "conflicts": snap.conflicts,
    })
}
