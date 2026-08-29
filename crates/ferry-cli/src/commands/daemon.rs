//! `ferry daemon`: watch folders, snapshot continuously, exchange with one
//! peer over TCP in the background using the unified `SyncEngine`.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
        let _lock = ferry_platform::ProcessLock::acquire(&home).map_err(|e| match e {
            ferry_platform::ProcessLockError::AlreadyRunning(pid) => {
                let pid_str = pid.map(|p| format!(" (PID {p})")).unwrap_or_default();
                CliError::new(
                    "daemon-already-running",
                    format!("A Ferry daemon is already running{pid_str}"),
                    "run `ferry daemon stop` first or check active processes",
                )
            }
            ferry_platform::ProcessLockError::Io(err) => {
                CliError::new("io", err.to_string(), "check permissions on FERRY_HOME")
            }
        })?;

        let identity = crate::ensure_identity()?;
        let mut supervisor =
            ferry_daemon::supervisor::Supervisor::new(home.clone(), identity.clone());
        for p in args.folders {
            let abs = if p.is_relative() {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p))
                    .unwrap_or_else(|_| p.clone())
            } else {
                p.clone()
            };
            if abs.as_os_str().is_empty() {
                continue;
            }
            match supervisor.handle_register(abs.clone()) {
                Ok(rec) => eprintln!("registered {} -> {}", rec.path.display(), rec.folder_id),
                Err(e) if e.code == "already-synced" => {
                    eprintln!("already-synced {}: {}", p.display(), e.message)
                }
                Err(e) => {
                    return Err(CliError::new(
                        Box::leak(e.code.into_boxed_str()),
                        e.message,
                        e.hint,
                    ))
                }
            }
        }
        supervisor.spawn_engines().map_err(|e| {
            CliError::new(
                Box::leak(e.code.into_boxed_str()),
                e.message,
                "check daemon log",
            )
        })?;
        let socket_path = ferry_ipc::paths::default_socket_path();
        if let Some(parent) = socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| {
                CliError::new("runtime-error", e.to_string(), "failed to start runtime")
            })?;
        rt.block_on(async move {
            let sup_arc = std::sync::Arc::new(tokio::sync::Mutex::new(supervisor));
            let ipc_handle = ferry_daemon::ipc::spawn_supervisor_ipc_server(
                socket_path.clone(),
                std::sync::Arc::clone(&sup_arc),
            )
            .map_err(|e| e.to_string())
            .expect("ipc bind");
            eprintln!("ferry device daemon listening at {}", socket_path.display());

            let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
            let s_tx = shutdown_tx.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = s_tx.send(true);
            });
            #[cfg(unix)]
            {
                let s_tx2 = shutdown_tx.clone();
                tokio::spawn(async move {
                    if let Ok(mut sig) =
                        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    {
                        sig.recv().await;
                        let _ = s_tx2.send(true);
                    }
                });
            }

            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut sup = sup_arc.lock().await;
                        sup.tick();
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            eprintln!("Shutting down ferry daemon cleanly...");
                            break;
                        }
                    }
                }
            }
            ipc_handle.shutdown();
        });
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
    let home = crate::home::ferry_home()?;
    let pid_file = home.join("daemon.pid");
    let socket_path = ferry_ipc::paths::default_socket_path();

    let pid = if pid_file.is_file() {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
    } else {
        None
    };

    if let Some(pid) = pid {
        #[cfg(unix)]
        {
            let res = unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            if res == 0 {
                // Wait up to 2 seconds for process to exit
                for _ in 0..20 {
                    std::thread::sleep(Duration::from_millis(100));
                    let probe = unsafe { libc::kill(pid as i32, 0) };
                    if probe != 0 {
                        break;
                    }
                }
            }
        }
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(&socket_path);
        return Ok(Output::new(
            serde_json::json!({"command": "daemon", "action": "stop", "status": "stopped", "pid": pid}),
            format!("Ferry daemon (PID {pid}) stopped.\n"),
        ));
    }

    if socket_path.exists() {
        let _ = std::fs::remove_file(&socket_path);
    }

    Ok(Output::new(
        serde_json::json!({"command": "daemon", "action": "stop", "status": "not_running"}),
        "No Ferry daemon is running.\n",
    ))
}

pub fn status() -> CliResult<Output> {
    let home = crate::home::ferry_home()?;
    let pid_file = home.join("daemon.pid");
    let socket_path = ferry_ipc::paths::default_socket_path();

    let pid = if pid_file.is_file() {
        std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
    } else {
        None
    };

    let is_alive = if let Some(pid) = pid {
        #[cfg(unix)]
        {
            unsafe { libc::kill(pid as i32, 0) == 0 }
        }
        #[cfg(not(unix))]
        {
            true
        }
    } else {
        false
    };

    if is_alive {
        let pid = pid.unwrap();
        Ok(Output::new(
            serde_json::json!({"command": "daemon", "action": "status", "status": "running", "pid": pid, "socket": socket_path}),
            format!(
                "Ferry daemon is running (PID {pid}, socket: {})\n",
                socket_path.display()
            ),
        ))
    } else {
        Ok(Output::new(
            serde_json::json!({"command": "daemon", "action": "status", "status": "stopped"}),
            "No Ferry daemon is running.\n",
        ))
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
