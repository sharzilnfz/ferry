//! IPC client query helpers for talking to a running Ferry daemon.
//!
//! Provides fast queries for cached snapshots, commands, and conflicts,
//! with automatic background daemon bootstrapping.

use std::path::Path;
use std::time::Duration;

use ferry_ipc::paths::{default_socket_path, socket_path_for_dir};
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, EngineSnapshot};
use ferry_ipc::IpcClient;

use crate::error::CliError;

/// Short timeout for local IPC socket operations (connect, query, command).
pub const IPC_TIMEOUT: Duration = Duration::from_millis(500);

/// Check if the global Ferry daemon is running and responsive.
#[must_use]
pub fn is_daemon_running() -> bool {
    let socket_path = default_socket_path();
    run_async(Duration::from_millis(200), async move {
        let mut conn = IpcClient::connect(&socket_path).await.ok()?;
        let _ = tokio::time::timeout(Duration::from_millis(50), conn.recv_message()).await;
        conn.send_command(&ClientCommand::Ping).await.ok()?;
        let resp = tokio::time::timeout(Duration::from_millis(150), conn.recv_message())
            .await
            .ok()?
            .ok()?;
        match resp {
            Some(
                DaemonMessage::Pong
                | DaemonMessage::Snapshot(_)
                | DaemonMessage::Ack { .. },
            ) => Some(true),
            _ => None,
        }
    })
    .unwrap_or(false)
}

/// Ensure the background Ferry daemon is running for the current `$FERRY_HOME`.
///
/// If not already running, auto-spawns `ferry daemon` in the background,
/// waits for socket readiness, and returns Ok(()).
pub fn ensure_daemon_running() -> Result<(), CliError> {
    if is_daemon_running() {
        return Ok(());
    }

    let home = crate::home::ferry_home()?;
    std::fs::create_dir_all(&home).map_err(|e| {
        CliError::new(
            "io",
            format!("failed to create ferry home directory {}: {e}", home.display()),
            "check permissions and disk space",
        )
    })?;

    let socket_path = default_socket_path();
    #[cfg(unix)]
    {
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    let exe = std::env::var_os("CARGO_BIN_EXE_ferry")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe().ok().and_then(|mut p| {
                if p.to_string_lossy().contains("deps") {
                    p.pop();
                    p.pop();
                    p.push("ferry");
                    if p.exists() {
                        Some(p)
                    } else {
                        None
                    }
                } else {
                    Some(p)
                }
            })
        })
        .unwrap_or_else(|| std::path::PathBuf::from("ferry"));
    let log_path = home.join("daemon.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon");
    cmd.env("FERRY_HOME", &home);

    if let Some(f) = log_file {
        if let Ok(f_err) = f.try_clone() {
            cmd.stdout(std::process::Stdio::from(f));
            cmd.stderr(std::process::Stdio::from(f_err));
        } else {
            cmd.stdout(std::process::Stdio::from(f));
            cmd.stderr(std::process::Stdio::null());
        }
    } else {
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
    }
    cmd.stdin(std::process::Stdio::null());

    let mut child = cmd.spawn().map_err(|e| {
        CliError::new(
            "daemon-spawn",
            format!("failed to spawn background daemon: {e}"),
            "run `ferry daemon` manually or check executable permissions",
        )
    })?;

    // Wait for socket readiness up to 5 seconds
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(CliError::new(
                "daemon-exited",
                format!("background daemon exited prematurely with status {status}"),
                format!("inspect log file at {}", log_path.display()),
            ));
        }
        if is_daemon_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if is_daemon_running() {
        Ok(())
    } else {
        Err(CliError::new(
            "daemon-timeout",
            "timed out waiting for background daemon socket to become ready",
            format!("inspect log file at {}", log_path.display()),
        ))
    }
}

/// Query the running daemon for an instant engine snapshot over IPC.
///
/// Returns `Some(EngineSnapshot)` if a daemon socket is alive and responds within the timeout,
/// or `None` if the daemon is offline, socket is missing, connection fails, or query times out.
pub fn query_status(folder: &Path) -> Option<EngineSnapshot> {
    let folder_socket = socket_path_for_dir(folder);
    if let Some(snap) = query_status_at(&folder_socket) {
        return Some(snap);
    }
    let default_socket = default_socket_path();
    query_status_at(&default_socket)
}

fn query_status_at(socket_path: &Path) -> Option<EngineSnapshot> {
    let p = socket_path.to_path_buf();
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&p).await.ok()?;

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

/// Send a typed command to the daemon over IPC and await the response.
pub fn send_command(folder: &Path, cmd: ClientCommand) -> Option<DaemonMessage> {
    let socket_path = socket_path_for_dir(folder);
    if let Some(resp) = send_command_at(&socket_path, &cmd) {
        return Some(resp);
    }
    let default_socket = default_socket_path();
    send_command_at(&default_socket, &cmd)
}

/// Send a typed command to the default global device daemon over IPC.
pub fn send_command_to_daemon(cmd: ClientCommand) -> Option<DaemonMessage> {
    let socket_path = default_socket_path();
    send_command_at(&socket_path, &cmd)
}

fn send_command_at(socket_path: &Path, cmd: &ClientCommand) -> Option<DaemonMessage> {
    let p = socket_path.to_path_buf();
    let command = cmd.clone();
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&p).await.ok()?;
        conn.send_command(&command).await.ok()?;
        while let Ok(Ok(Some(msg))) = tokio::time::timeout(IPC_TIMEOUT, conn.recv_message()).await {
            if matches!(&command, ClientCommand::GetStatus) || !matches!(&msg, DaemonMessage::Snapshot(_)) {
                return Some(msg);
            }
        }
        None
    })
}

/// Query conflict list from the running daemon over IPC.
pub fn query_conflicts(folder: &Path) -> Option<Vec<ferry_sync_engine::ConflictEntry>> {
    let socket_path = socket_path_for_dir(folder);
    if let Some(confs) = query_conflicts_at(&socket_path) {
        return Some(confs);
    }
    let default_socket = default_socket_path();
    query_conflicts_at(&default_socket)
}

fn query_conflicts_at(socket_path: &Path) -> Option<Vec<ferry_sync_engine::ConflictEntry>> {
    let p = socket_path.to_path_buf();
    run_async(IPC_TIMEOUT, async move {
        let mut conn = IpcClient::connect(&p).await.ok()?;
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
