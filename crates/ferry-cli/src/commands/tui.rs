use std::path::Path;
use std::sync::Arc;

use ferry_ipc::backend::{connect_auto, UiBackend};

use crate::error::CliResult;

pub fn run(folder: Option<&Path>) -> CliResult<crate::out::Output> {
    if let Ok(home) = crate::home::ferry_home() {
        let _ = crate::bootstrap::ensure_daemon(&home);
    }

    let rt = tokio::runtime::Runtime::new().map_err(|e| {
        crate::error::CliError::new(
            "runtime-error",
            e.to_string(),
            "failed to start async runtime",
        )
    })?;
    rt.block_on(async {
        let dir = folder.unwrap_or_else(|| Path::new("."));
        let socket_path = ferry_ipc::paths::socket_path_for_dir(dir);
        let backend: Arc<dyn UiBackend> = Arc::new(connect_auto(socket_path, dir.to_path_buf()));

        let mut guard = ferry_tui::TerminalGuard::init().map_err(|e| {
            crate::error::CliError::new("tui-error", e.to_string(), "failed to initialize terminal")
        })?;
        let events = ferry_tui::TerminalEvents::new();
        let mut app = ferry_tui::TuiApp::new_with_backend(backend.clone());
        app.run(guard.terminal_mut(), backend, events)
            .await
            .map_err(|e| {
                crate::error::CliError::new("tui-error", e.to_string(), "TUI exited with an error")
            })?;
        Ok(crate::out::Output::new(
            serde_json::json!({ "status": "closed" }),
            "TUI closed.",
        ))
    })
}
