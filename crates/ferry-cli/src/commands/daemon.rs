//! `ferry daemon`: watch folders, snapshot continuously, exchange with one
//! peer over TCP in the background using the unified `SyncEngine`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ferry_platform::{DaemonLockError, TerminateOutcome, TERMINATE_DEADLINE};
use ferry_store::format::hex;
use ferry_sync::{EngineConfig, SyncEngine};

use crate::error::{CliError, CliResult};
use crate::folder;
use crate::out::Output;

pub struct DaemonArgs<'a> {
    pub folders: &'a [PathBuf],
    pub listen: Option<&'a str>,
    pub peer_url: Option<&'a str>,
    pub transport: &'a str,
    pub interval_secs: u64,
    pub json: bool,
}

pub fn run(args: DaemonArgs<'_>) -> CliResult<Output> {
    check_transport(args.transport)?;

    let listen_addr: Option<SocketAddr> = match args.listen {
        Some(s) => Some(s.parse().map_err(|_| {
            CliError::new(
                "bad-address",
                format!("--listen {s:?} is not HOST:PORT"),
                "example: 127.0.0.1:44001",
            )
        })?),
        None => None,
    };
    let peer_addr: Option<SocketAddr> = match args.peer_url {
        Some(s) => Some(s.parse().map_err(|_| {
            CliError::new(
                "bad-address",
                format!("--peer-url {s:?} is not HOST:PORT"),
                "example: 127.0.0.1:44001",
            )
        })?),
        None => None,
    };
    if listen_addr.is_none() && peer_addr.is_none() {
        let home = crate::home::ferry_home()?;
        let identity = crate::ensure_identity()?;
        ferry_daemon::device_daemon::run(&home, identity, args.folders)
            .map_err(render_daemon_error)?;
        return Ok(Output::new(
            serde_json::json!({"command":"daemon","status":"stopped"}),
            "Daemon stopped.\n",
        ));
    }

    let paths: Vec<PathBuf> = if args.folders.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.folders.to_vec()
    };

    let transport: Arc<dyn ferry_sync::Transport> = Arc::new(ferry_sync::TcpTransport);
    let identity = crate::ensure_identity()?;
    let device_id = *identity.public();
    let mut handles = Vec::with_capacity(paths.len());
    let mut ipc_handles = Vec::with_capacity(paths.len());

    for (idx, p) in paths.iter().enumerate() {
        let opened = folder::open_folder(p)?;
        let poly = ferry_store::chunker::ValidatedPoly::try_from(opened.poly).map_err(|e| {
            CliError::new(
                "poly-invalid",
                e.to_string(),
                format!(
                    "the polynomial record for {} is corrupt; restore the store from a known-good backup",
                    opened.root.display()
                ),
            )
        })?;

        let bind = if idx == 0 { listen_addr } else { None };
        let tag = format!("ferry-{}", &hex(&device_id)[..8]);

        let cfg = EngineConfig {
            tag,
            store_dir: opened.root.clone(),
            tree_dir: opened.root.clone(),
            poly,
            folder_id: opened.folder_id,
            poll_interval: Duration::from_millis(200),
            opportunistic_every: (args.interval_secs * 5).max(1) as u32,
            bind_addr: bind,
            connect_to: peer_addr,
            expected_peer_id: None,
            pin_state_dir: Some(opened.state_dir()),
            quiet: true,
        };

        let mut engine = SyncEngine::with_store(cfg, transport.clone(), Arc::clone(&opened.store))
            .map_err(|e| {
                CliError::new(
                    "bind",
                    format!(
                        "cannot initialize engine for {}: {e}",
                        opened.root.display()
                    ),
                    "pick another port or free the existing listener",
                )
            })?;
        // Same as `ferry sync`: run sessions under the real FERRY_HOME
        // identity so CONFIG_HEAD-seeded allow-lists recognize this device.
        engine.set_identity(identity.clone());

        if let Some(addr) = engine.listen_addr() {
            println!("LISTENING {addr}");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }

        let handle = engine.start();

        let (broadcast_tx, _) = tokio::sync::broadcast::channel(128);
        let daemon_state = Arc::new(ferry_daemon::state::DaemonState::new(
            handle.clone(),
            opened.root.clone(),
            opened.root.clone(),
            opened.folder_id,
            identity.clone(),
            broadcast_tx,
        ));

        let socket_path = ferry_ipc::paths::socket_path_for_dir(&opened.root);
        let ipc_handle =
            ferry_daemon::ipc::spawn_ipc_server(socket_path, Arc::clone(&daemon_state)).map_err(
                |e| {
                    CliError::new(
                        "ipc-server",
                        format!("cannot bind IPC server for {}: {e}", opened.root.display()),
                        "check socket permissions or remove stale socket",
                    )
                },
            )?;
        ipc_handles.push(ipc_handle);
        handles.push(handle);
    }

    if let Some(first) = handles.first() {
        first.join_until_signal();
    }

    for h in ipc_handles {
        h.shutdown();
    }

    Ok(Output::new(
        serde_json::json!({"command": "daemon", "status": "stopped"}),
        "Daemon stopped.\n",
    ))
}

pub fn stop() -> CliResult<Output> {
    stop_in(&crate::home::ferry_home()?)
}

/// Stop whatever daemon `home` records. A pure function of the directory:
/// termination, liveness polling, and PID-file ownership live in
/// ferry-platform; this only renders outcomes.
pub fn stop_in(home: &Path) -> CliResult<Output> {
    stop_with_deadline(home, TERMINATE_DEADLINE)
}

/// The socket the central daemon binds for `home`: `<home>/daemon.sock`
/// on Unix, the fixed named pipe on Windows. Derived from the directory
/// so stop and status stay pure functions of it.
fn socket_path_for_home(home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let _ = home;
        ferry_ipc::paths::default_socket_path()
    }
    #[cfg(not(windows))]
    {
        home.join(ferry_ipc::paths::DEFAULT_SOCKET_FILENAME)
    }
}

fn stop_with_deadline(home: &Path, deadline: Duration) -> CliResult<Output> {
    let socket_path = socket_path_for_home(home);
    match ferry_platform::terminate(home, deadline) {
        Ok(TerminateOutcome::Stopped { pid }) => {
            let _ = std::fs::remove_file(&socket_path);
            Ok(Output::new(
                serde_json::json!({"command": "daemon", "action": "stop", "status": "stopped", "pid": pid}),
                format!("Ferry daemon (PID {pid}) stopped.\n"),
            ))
        }
        Ok(TerminateOutcome::NotRunning) => {
            let _ = std::fs::remove_file(&socket_path);
            Ok(Output::new(
                serde_json::json!({"command": "daemon", "action": "stop", "status": "not_running"}),
                "No Ferry daemon is running.\n",
            ))
        }
        Ok(TerminateOutcome::Timeout { pid }) => Err(CliError::new(
            "daemon-stop-timeout",
            format!(
                "Ferry daemon (PID {pid}) did not exit within {}s",
                deadline.as_secs()
            ),
            "the daemon is still running; inspect it with `ferry daemon status` and escalate manually",
        )),
        Err(DaemonLockError::AlreadyRunning(pid)) => Err(CliError::new(
            "daemon-already-running",
            format!(
                "A Ferry daemon is already running{}",
                pid.map(|p| format!(" (PID {p})")).unwrap_or_default()
            ),
            "run `ferry daemon stop` first or check active processes",
        )),
        Err(DaemonLockError::Io(err)) => {
            Err(CliError::new("io", err.to_string(), "check permissions on FERRY_HOME"))
        }
    }
}

pub fn status() -> CliResult<Output> {
    status_in(&crate::home::ferry_home()?)
}

/// Report whether the daemon `home` records is alive. A pure function of
/// the directory: liveness is the platform's start-token check, never a
/// blind pid probe.
pub fn status_in(home: &Path) -> CliResult<Output> {
    let socket_path = socket_path_for_home(home);
    match ferry_platform::running_pid(home) {
        Some(pid) => Ok(Output::new(
            serde_json::json!({"command": "daemon", "action": "status", "status": "running", "pid": pid, "socket": socket_path}),
            format!(
                "Ferry daemon is running (PID {pid}, socket: {})\n",
                socket_path.display()
            ),
        )),
        None => Ok(Output::new(
            serde_json::json!({"command": "daemon", "action": "status", "status": "stopped"}),
            "No Ferry daemon is running.\n",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A SIGTERM-immune stand-in daemon, so stop's timeout path runs in
    /// milliseconds instead of the real five-second deadline.
    fn spawn_term_immune_sleeper() -> std::process::Child {
        let child = Command::new("sh")
            .args(["-c", "trap \"\" TERM; sleep 30"])
            .spawn()
            .expect("spawn TERM-immune sleeper");
        // Let the trap install before anything signals the child.
        std::thread::sleep(Duration::from_millis(100));
        child
    }

    fn stamp(pid: u32, token: Option<u64>) -> String {
        match token {
            Some(token) => format!("{pid} {token}\n"),
            None => format!("{pid}\n"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn stop_timeout_is_a_coded_error_that_preserves_the_pid_file() {
        let home = tempfile::tempdir().unwrap();
        let mut child = spawn_term_immune_sleeper();
        let token = ferry_platform::process_start_token(child.id());
        std::fs::write(home.path().join("daemon.pid"), stamp(child.id(), token)).unwrap();

        let err = stop_with_deadline(home.path(), Duration::from_millis(300)).unwrap_err();

        assert_eq!(err.code, "daemon-stop-timeout");
        assert_eq!(
            err.exit_code(),
            4,
            "CI asserts the distinct timeout exit code"
        );
        assert!(
            home.path().join("daemon.pid").is_file(),
            "pid file preserved so status can report the live daemon"
        );

        let status = status_in(home.path()).unwrap();
        assert_eq!(status.json["status"], "running");
        assert_eq!(status.json["pid"].as_u64(), Some(u64::from(child.id())));

        child.kill().expect("kill sleeper");
        child.wait().expect("reap sleeper");
    }
}

fn check_transport(kind: &str) -> CliResult<()> {
    match kind {
        "tcp" | "iroh" => Ok(()),
        other => Err(CliError::new(
            "transport-unavailable",
            format!("transport {other:?} is not implemented yet"),
            "use --transport tcp or --transport iroh",
        )),
    }
}

/// Render the daemon entry's typed failures as coded CLI errors. The codes,
/// messages, and hints here are the CLI's stable output contract.
fn render_daemon_error(e: ferry_daemon::device_daemon::DeviceDaemonError) -> CliError {
    use ferry_daemon::device_daemon::DeviceDaemonError as DaemonError;
    match e {
        DaemonError::AlreadyRunning { pid } => CliError::new(
            "daemon-already-running",
            format!(
                "A Ferry daemon is already running{}",
                pid.map(|p| format!(" (PID {p})")).unwrap_or_default()
            ),
            "run `ferry daemon stop` first or check active processes",
        ),
        DaemonError::Lock(err) => {
            CliError::new("io", err.to_string(), "check permissions on FERRY_HOME")
        }
        DaemonError::Register { error, .. } => CliError::new(error.code, error.message, error.hint),
        DaemonError::Spawn { code, message } => CliError::new(code, message, "check daemon log"),
        DaemonError::Runtime(err) => {
            CliError::new("runtime-error", err.to_string(), "failed to start runtime")
        }
        DaemonError::Ipc(message) => CliError::new(
            "ipc-server",
            message,
            "check socket permissions or remove stale socket",
        ),
    }
}
