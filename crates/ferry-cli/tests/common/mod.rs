//! Shared fixture: isolated `FERRY_HOME` per test, serialized across threads
//! because command functions read the process env internally.

pub struct Env {
    _home: tempfile::TempDir,
    _work: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Env {
    /// Acquire the env lock for the whole test body and point `FERRY_HOME` at
    /// a fresh temp dir.
    pub fn new(_label: &str) -> Env {
        let guard =
            std::sync::Mutex::lock(&ENV_LOCK).unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        std::env::set_var("FERRY_HOME", home.path());
        Env {
            _home: home,
            _work: work,
            _guard: guard,
        }
    }

    pub fn work(&self) -> std::path::PathBuf {
        self._work.path().to_path_buf()
    }

    /// Point `FERRY_HOME` somewhere else (second simulated device). Caller
    /// keeps the original Env alive so the lock is held throughout.
    #[allow(dead_code)]
    #[allow(clippy::unused_self)] // method shape keeps fixture call sites uniform
    pub fn switch_home_to(&self, dir: &std::path::Path) {
        std::env::set_var("FERRY_HOME", dir);
    }
}

#[allow(dead_code)]
pub struct RunningDaemon {
    pub engine_handle: ferry_sync::EngineHandle,
    pub state: std::sync::Arc<ferry_daemon::state::DaemonState>,
    pub ipc_handle: Option<ferry_daemon::ipc::IpcServerHandle>,
}

#[allow(dead_code)]
impl RunningDaemon {
    #[allow(deprecated)]
    pub fn spawn_with_ipc(proj: &std::path::Path) -> Self {
        let opened = ferry_cli::folder::open_folder(proj).expect("open folder");
        let identity = ferry_cli::ensure_identity().expect("device identity");

        let mut cfg = ferry_sync::EngineConfig::default_for_test(12345);
        cfg.tag = "ipc-test-daemon".to_string();
        cfg.store_dir.clone_from(&opened.root);
        cfg.tree_dir.clone_from(&opened.root);
        cfg.folder_id = opened.folder_id;
        cfg.pin_state_dir = Some(opened.state_dir());
        cfg.poll_interval = std::time::Duration::from_millis(50);

        let mut engine = ferry_sync::SyncEngine::with_store(
            cfg,
            std::sync::Arc::new(ferry_sync::TcpTransport),
            std::sync::Arc::clone(&opened.store),
        )
        .expect("engine init");
        engine.set_identity(identity.clone());
        let handle = engine.start();

        let (broadcast_tx, _) = tokio::sync::broadcast::channel(128);
        let daemon_state = std::sync::Arc::new(ferry_daemon::state::DaemonState::new(
            handle.clone(),
            opened.root.clone(),
            opened.root.clone(),
            opened.folder_id,
            identity,
            broadcast_tx,
        ));

        let socket_path = ferry_ipc::paths::socket_path_for_dir(&opened.root);
        let ipc_handle =
            ferry_daemon::ipc::spawn_ipc_server(socket_path, std::sync::Arc::clone(&daemon_state))
                .expect("spawn ipc server");

        Self {
            engine_handle: handle,
            state: daemon_state,
            ipc_handle: Some(ipc_handle),
        }
    }

    #[allow(deprecated)]
    pub fn start(proj: &std::path::Path) -> Self {
        let opened = ferry_cli::folder::open_folder(proj).expect("open folder");
        let identity = ferry_cli::ensure_identity().expect("device identity");

        let mut cfg = ferry_sync::EngineConfig::default_for_test(12345);
        cfg.tag = "ipc-test-daemon".to_string();
        cfg.store_dir.clone_from(&opened.root);
        cfg.tree_dir.clone_from(&opened.root);
        cfg.folder_id = opened.folder_id;
        cfg.pin_state_dir = Some(opened.state_dir());
        cfg.poll_interval = std::time::Duration::from_millis(50);

        let mut engine = ferry_sync::SyncEngine::with_store(
            cfg,
            std::sync::Arc::new(ferry_sync::TcpTransport),
            std::sync::Arc::clone(&opened.store),
        )
        .expect("engine init");
        engine.set_identity(identity.clone());
        let handle = engine.start();

        let (broadcast_tx, _) = tokio::sync::broadcast::channel(128);
        let daemon_state = std::sync::Arc::new(ferry_daemon::state::DaemonState::new(
            handle.clone(),
            opened.root.clone(),
            opened.root.clone(),
            opened.folder_id,
            identity,
            broadcast_tx,
        ));

        let socket_path = ferry_ipc::paths::socket_path_for_dir(&opened.root);
        let ipc_handle =
            ferry_daemon::ipc::spawn_ipc_server(socket_path, std::sync::Arc::clone(&daemon_state))
                .expect("spawn ipc server");

        Self {
            engine_handle: handle,
            state: daemon_state,
            ipc_handle: Some(ipc_handle),
        }
    }

    pub fn stop_ipc(&mut self) {
        if let Some(h) = self.ipc_handle.take() {
            h.shutdown();
        }
    }
}

impl Drop for RunningDaemon {
    fn drop(&mut self) {
        self.stop_ipc();
        self.engine_handle.shutdown();
    }
}
