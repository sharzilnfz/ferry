


use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ferry_platform::{DaemonLockError, TerminateOutcome, TERMINATE_DEADLINE};

use crate::error::{CliError, CliResult};
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
    let home = crate::home::ferry_home()?;
    let identity = crate::ensure_identity()?;

    if listen_addr.is_none() && peer_addr.is_none() {
        ferry_daemon::device_daemon::run(&home, identity, args.folders)
            .map_err(render_daemon_error)?;
        return Ok(Output::new(
            serde_json::json!({"command":"daemon","status":"stopped"}),
            "Daemon stopped.\n",
        ));
    }

    let adhoc_args = ferry_daemon::device_daemon::AdHocDaemonArgs {
        folders: args.folders.to_vec(),
        listen_addr,
        peer_addr,
        transport: args.transport.to_string(),
        interval_secs: args.interval_secs,
    };

    ferry_daemon::device_daemon::run_adhoc(&home, identity, adhoc_args)
        .map_err(render_daemon_error)?;

    Ok(Output::new(
        serde_json::json!({"command": "daemon", "status": "stopped"}),
        "Daemon stopped.\n",
    ))
}

pub fn stop() -> CliResult<Output> {
    stop_in(&crate::home::ferry_home()?)
}




pub fn stop_in(home: &Path) -> CliResult<Output> {
    stop_with_deadline(home, TERMINATE_DEADLINE)
}




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

    
    
    fn spawn_term_immune_sleeper() -> std::process::Child {
        let child = Command::new("sh")
            .args(["-c", "trap \"\" TERM; sleep 30"])
            .spawn()
            .expect("spawn TERM-immune sleeper");
        
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
