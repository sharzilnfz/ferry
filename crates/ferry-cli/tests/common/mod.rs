//! Shared fixture: isolated FERRY_HOME per test, serialized across threads
//! because command functions read the process env internally.

pub struct Env {
    _home: tempfile::TempDir,
    _work: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
}

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Env {
    /// Acquire the env lock for the whole test body and point FERRY_HOME at
    /// a fresh temp dir.
    pub fn new(_label: &str) -> Env {
        let guard = std::sync::Mutex::lock(&ENV_LOCK).unwrap_or_else(|p| p.into_inner());
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

    /// Point FERRY_HOME somewhere else (second simulated device). Caller
    /// keeps the original Env alive so the lock is held throughout.
    #[allow(dead_code)]
    pub fn switch_home_to(&self, dir: &std::path::Path) {
        std::env::set_var("FERRY_HOME", dir);
    }
}
