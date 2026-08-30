//! The one device-daemon entry point. Lock acquisition and teardown,
//! folder registration, engine spawning, the supervisor IPC server, signal
//! handling, and the supervision tick loop each exist here and nowhere
//! else; `ferry` and `ferry-sync` parse arguments and delegate.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_ipc::backend::OpError;
use ferry_platform::{DaemonLock, DaemonLockError};

/// Why the device daemon failed to start.
#[derive(Debug)]
pub enum DeviceDaemonError {
    /// Another daemon holds the home's lock.
    AlreadyRunning { pid: Option<u32> },
    /// Lock acquisition failed on I/O.
    Lock(std::io::Error),
    /// A folder failed to register; `error` carries the coded cause.
    Register { path: PathBuf, error: Box<OpError> },
    /// Registered folders failed to spawn engines.
    Spawn { code: String, message: String },
    /// The async runtime could not start.
    Runtime(std::io::Error),
    /// The supervisor IPC server could not bind.
    Ipc(String),
}

impl std::fmt::Display for DeviceDaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning { pid } => {
                let pid_str = pid.map(|p| format!(" (PID {p})")).unwrap_or_default();
                write!(
                    f,
                    "A Ferry daemon is already running{pid_str}. Run `ferry daemon stop` first."
                )
            }
            Self::Lock(err) => write!(f, "Failed to acquire daemon lock: {err}"),
            Self::Register { path, error } => {
                write!(f, "register {}: {}", path.display(), error.message)
            }
            Self::Spawn { code, message } => write!(f, "spawn engines: {code}: {message}"),
            Self::Runtime(err) => write!(f, "tokio runtime: {err}"),
            Self::Ipc(message) => write!(f, "ipc server: {message}"),
        }
    }
}

impl std::error::Error for DeviceDaemonError {}

/// Run the device daemon for `home` until SIGINT or SIGTERM, then tear down
/// cleanly. Registers `folders` into the supervisor before spawning engines.
/// The lock is held for the duration; dropping it removes the PID file.
pub fn run(
    home: &Path,
    identity: DeviceIdentity,
    folders: &[PathBuf],
) -> Result<(), DeviceDaemonError> {
    let _lock = DaemonLock::acquire(home).map_err(|e| match e {
        DaemonLockError::AlreadyRunning(pid) => DeviceDaemonError::AlreadyRunning { pid },
        DaemonLockError::Io(err) => DeviceDaemonError::Lock(err),
    })?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(DeviceDaemonError::Runtime)?;

    let mut supervisor = crate::supervisor::Supervisor::new(home.to_path_buf(), identity);
    for folder in folders {
        let abs = if folder.is_relative() {
            std::env::current_dir().map_or_else(|_| folder.clone(), |cwd| cwd.join(folder))
        } else {
            folder.clone()
        };
        if abs.as_os_str().is_empty() {
            continue;
        }
        match rt.block_on(async { supervisor.handle_register(abs) }) {
            Ok(rec) => eprintln!("registered {} -> {}", rec.path.display(), rec.folder_id),
            Err(e) if e.code == "already-synced" => {
                eprintln!("already-synced {}: {}", folder.display(), e.message);
            }
            Err(e) => {
                return Err(DeviceDaemonError::Register {
                    path: folder.clone(),
                    error: Box::new(e),
                });
            }
        }
    }
    rt.block_on(async { supervisor.spawn_engines() })
        .map_err(|e| DeviceDaemonError::Spawn {
            code: e.code,
            message: e.message,
        })?;

    let socket_path = ferry_ipc::paths::default_socket_path();
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let supervisor = Arc::new(tokio::sync::Mutex::new(supervisor));
    let ipc_handle = rt
        .block_on(async {
            crate::ipc::spawn_supervisor_ipc_server(socket_path.clone(), Arc::clone(&supervisor))
        })
        .map_err(|e| DeviceDaemonError::Ipc(e.to_string()))?;
    eprintln!("ferry device daemon listening at {}", socket_path.display());

    rt.block_on(async {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        tokio::spawn({
            let shutdown_tx = shutdown_tx.clone();
            async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = shutdown_tx.send(true);
            }
        });
        #[cfg(unix)]
        tokio::spawn({
            let shutdown_tx = shutdown_tx.clone();
            async move {
                if let Ok(mut sig) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    sig.recv().await;
                    let _ = shutdown_tx.send(true);
                }
            }
        });

        let mut interval = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                _ = interval.tick() => supervisor.lock().await.tick(),
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        eprintln!("Shutting down ferry daemon cleanly...");
                        break;
                    }
                }
            }
        }
    });
    ipc_handle.shutdown();
    Ok(())
}
