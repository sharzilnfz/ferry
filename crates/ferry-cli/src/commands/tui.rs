use crate::error::CliResult;
use std::path::Path;

pub fn run(folder: Option<&Path>) -> CliResult<crate::out::Output> {
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
        let conn = ferry_ipc::IpcClient::connect(&socket_path)
            .await
            .map_err(|e| {
                crate::error::CliError::new(
                    "daemon-offline",
                    format!(
                        "failed to connect to daemon at {}: {}",
                        socket_path.display(),
                        e
                    ),
                    "ensure ferry daemon is running for this folder",
                )
            })?;

        let mut guard = ferry_tui::TerminalGuard::init().map_err(|e| {
            crate::error::CliError::new("tui-error", e.to_string(), "failed to initialize terminal")
        })?;
        let events = ferry_tui::TerminalEvents::new();
        let mut app = ferry_tui::TuiApp::default();
        app.run_with_connection(guard.terminal_mut(), conn, events)
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
