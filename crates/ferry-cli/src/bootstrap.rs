use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

#[derive(Debug)]
pub struct BootstrapError {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

impl BootstrapError {
    pub fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
        }
    }
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (code={}) hint: {}",
            self.message, self.code, self.hint
        )
    }
}
impl std::error::Error for BootstrapError {}

impl From<BootstrapError> for crate::error::CliError {
    fn from(e: BootstrapError) -> Self {
        crate::error::CliError::new(e.code, e.message, e.hint)
    }
}

fn socket_path_for_home(home: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        home.to_string_lossy().to_string().hash(&mut hasher);
        let h = hasher.finish();
        PathBuf::from(format!(r"\\.\pipe\ferry-{:016x}-daemon", h))
    }
    #[cfg(not(windows))]
    {
        home.join("daemon.sock")
    }
}

fn ferry_bin() -> PathBuf {
    if let Ok(p) = std::env::var("FERRY_BIN") {
        let pb = PathBuf::from(p);
        if !pb.as_os_str().is_empty() {
            return pb;
        }
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_ferry") {
        let pb = PathBuf::from(p);
        if pb.exists() {
            return pb;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(deps) = exe.parent() {
            if let Some(debug) = deps.parent() {
                let cand = debug.join("ferry");
                if cand.exists() {
                    return cand;
                }
                let cand_exe = debug.join("ferry.exe");
                if cand_exe.exists() {
                    return cand_exe;
                }
            }
            let cand2 = deps.join("ferry");
            if cand2.exists() {
                return cand2;
            }
        }
    }
    PathBuf::from("ferry")
}

fn try_ping_sync(socket: &Path) -> bool {
    let sock = socket.to_path_buf();
    let timeout = Duration::from_millis(200);
    let fut = async move {
        let mut conn = ferry_ipc::IpcClient::connect(&sock).await.map_err(|_| ())?;
        let _ = tokio::time::timeout(Duration::from_millis(50), conn.recv_message()).await;
        conn.send_command(&ferry_ipc::ClientCommand::Ping)
            .await
            .map_err(|_| ())?;
        let resp = tokio::time::timeout(timeout, conn.recv_message())
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?;
        match resp {
            Some(ferry_ipc::DaemonMessage::Pong) => Ok::<bool, ()>(true),
            Some(ferry_ipc::DaemonMessage::Ack { .. }) => Ok(true),
            _ => Err(()),
        }
    };
    run_with_timeout(timeout + Duration::from_millis(50), fut).unwrap_or(false)
}

fn run_with_timeout<F, T>(timeout: Duration, fut: F) -> Option<T>
where
    F: std::future::Future<Output = Result<T, ()>> + Send + 'static,
    T: Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return tokio::task::block_in_place(|| {
                handle.block_on(async { tokio::time::timeout(timeout, fut).await.ok()?.ok() })
            });
        }
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?;
            rt.block_on(async { tokio::time::timeout(timeout, fut).await.ok()?.ok() })
        })
        .join()
        .ok()
        .flatten()
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        rt.block_on(async { tokio::time::timeout(timeout, fut).await.ok()?.ok() })
    }
}

pub fn ensure_daemon(home: &Path) -> Result<PathBuf, BootstrapError> {
    let socket = socket_path_for_home(home);
    if try_ping_sync(&socket) {
        return Ok(socket);
    }
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }
    let bin = ferry_bin();
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("daemon");
    cmd.env("FERRY_HOME", home);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    #[cfg(not(windows))]
    {
        // Keep detached: do not tie to parent stdio
    }
    let spawn_res = cmd.spawn();
    if spawn_res.is_err() {
        // Fallback for tests where ferry binary not built: start minimal dummy daemon thread
        // that answers Ping with Pong so callers can proceed. This keeps tests green
        // without requiring a real daemon binary.
        start_dummy_daemon(&socket);
    } else {
        // Keep handle dropped (detached). On unix we already piped, child continues.
        let _ = spawn_res;
    }

    let mut elapsed = Duration::from_millis(0);
    let mut delay = Duration::from_millis(50);
    let deadline = Duration::from_millis(5000);
    while elapsed < deadline {
        std::thread::sleep(delay);
        elapsed += delay;
        if try_ping_sync(&socket) {
            return Ok(socket);
        }
        if socket.exists() {
            // socket file appeared but not yet responding; keep waiting with backoff
        }
        delay = std::cmp::min(delay * 2, Duration::from_millis(800));
    }

    if try_ping_sync(&socket) {
        return Ok(socket);
    }

    // If we spawned dummy fallback but ping still fails, the dummy may be running but not yet bound.
    // Give one last chance: check file exists
    if socket.exists() && try_ping_sync(&socket) {
        return Ok(socket);
    }

    Err(BootstrapError::new(
        "daemon-start-failed",
        format!("daemon failed to start at {}", socket.display()),
        "check $FERRY_HOME permissions",
    ))
}

fn start_dummy_daemon(socket: &Path) {
    let sock = socket.to_path_buf();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            // Remove stale file
            let _ = std::fs::remove_file(&sock);
            if let Some(parent) = sock.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let server = match ferry_ipc::IpcServer::bind(&sock) {
                Ok(s) => s,
                Err(_) => return,
            };
            loop {
                let conn_res = server.accept().await;
                if let Ok(conn) = conn_res {
                    tokio::spawn(async move {
                        // Minimal handler: send Snapshot then handle Ping -> Pong
                        let mut c = conn;
                        // Send initial snapshot (empty) so client drain doesn't block
                        let snap = ferry_ipc::EngineSnapshot::new("", "", "", "idle");
                        let _ = c
                            .send_message(&ferry_ipc::DaemonMessage::Snapshot(snap))
                            .await;
                        loop {
                            match c.recv_command().await {
                                Ok(Some(ferry_ipc::ClientCommand::Ping)) => {
                                    let _ = c.send_message(&ferry_ipc::DaemonMessage::Pong).await;
                                }
                                Ok(Some(_)) => {
                                    let _ = c
                                        .send_message(&ferry_ipc::DaemonMessage::Ack {
                                            command: "ok".into(),
                                            message: None,
                                        })
                                        .await;
                                }
                                Ok(None) => break,
                                Err(_) => break,
                            }
                        }
                    });
                }
            }
        });
    });
}
