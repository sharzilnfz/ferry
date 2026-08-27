//! Dynamic frontend switch and dispatch (`ferry ui`).
//!
//! Dispatches to Native Desktop GUI (`--gui`), Web Dashboard (`--web`), or
//! Terminal TUI (`--tui`) backed by `AutoBackend`. When no explicit flag is
//! passed, selects the preferred compiled frontend (GUI -> Web -> TUI).
//! When a requested frontend is excluded at compile time, fails gracefully
//! with `feature-disabled` code and actionable recompilation hints.

use std::path::{Path, PathBuf};
#[cfg(feature = "web-ui")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "web-ui")]
use std::net::SocketAddr;
#[cfg(feature = "web-ui")]
use std::time::Instant;

#[cfg(feature = "web-ui")]
use axum::Router;
#[cfg(feature = "web-ui")]
pub use ferry_daemon::ui::{
    extract_token, generate_token, is_token_valid, ApiError, DashboardServer, IpcBackend, OpError,
};
#[cfg(any(feature = "web-ui", feature = "gui", feature = "tui"))]
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::out::Output;

/// 10 minutes of inactivity before automatic web server shutdown.
pub const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);

pub struct UiArgs<'a> {
    pub folder: Option<&'a Path>,
    pub gui: bool,
    pub web: bool,
    pub tui: bool,
    pub host: &'a str,
    pub port: u16,
    pub no_open: bool,
    pub test: bool,
}

/// Shared server state preserved for backwards compatibility with tests and callers.
#[cfg(feature = "web-ui")]
#[derive(Clone, Debug)]
pub struct UiServerState {
    pub folder: PathBuf,
    pub token: String,
    pub last_activity: Arc<std::sync::Mutex<Instant>>,
}

#[cfg(feature = "web-ui")]
impl UiServerState {
    #[must_use]
    pub fn new(folder: PathBuf, token: String) -> Self {
        Self {
            folder,
            token,
            last_activity: Arc::new(std::sync::Mutex::new(Instant::now())),
        }
    }

    pub fn record_activity(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }

    #[must_use]
    pub fn idle_duration(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }
}

/// Construct the Axum router for the web UI backed by `DashboardServer` and `AutoBackend`.
#[cfg(feature = "web-ui")]
pub fn router(state: Arc<UiServerState>) -> Router {
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&state.folder);
    let backend = ferry_daemon::ui::AutoBackend::new(socket_path).with_fallback(state.folder.clone());
    let server = DashboardServer::new(Arc::new(backend))
        .with_token(&state.token)
        .with_inactivity_timeout(INACTIVITY_TIMEOUT);
    server.router()
}

/// Platform-specific browser opener.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start"]);
        c
    };

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        cmd.arg(url);
        if let Err(e) = cmd.spawn() {
            eprintln!("Warning: failed to open browser automatically: {e}");
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
    }
}

#[cfg(feature = "gui")]
fn run_gui_mode(folder_path: &Path, test_mode: bool) -> CliResult<Output> {
    if test_mode {
        return Ok(Output::new(
            json!({
                "command": "ui",
                "frontend": "gui",
                "status": "ok",
                "folder": folder_path.display().to_string(),
            }),
            format!("Ferry GUI initialized successfully in test mode for {}\n", folder_path.display()),
        ));
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| {
            CliError::new(
                "runtime-error",
                e.to_string(),
                "failed to start async runtime for GUI",
            )
        })?;

    let socket_path = ferry_ipc::paths::socket_path_for_dir(folder_path);
    let backend: Arc<dyn ferry_ipc::backend::UiBackend> = Arc::new(
        ferry_daemon::ui::AutoBackend::new(socket_path).with_fallback(folder_path.to_path_buf()),
    );

    let handle = rt.handle().clone();
    ferry_gui::run_gui(backend, handle).map_err(|e| {
        CliError::new("gui-error", e.to_string(), "GUI application exited with error")
    })?;

    Ok(Output::new(
        json!({
            "command": "ui",
            "frontend": "gui",
            "status": "closed",
        }),
        "Ferry GUI closed.\n",
    ))
}

#[cfg(feature = "tui")]
fn run_tui_mode(folder_path: &Path, test_mode: bool) -> CliResult<Output> {
    if test_mode {
        return Ok(Output::new(
            json!({
                "command": "ui",
                "frontend": "tui",
                "status": "ok",
                "folder": folder_path.display().to_string(),
            }),
            format!("Ferry TUI initialized successfully in test mode for {}\n", folder_path.display()),
        ));
    }
    crate::commands::tui::run(Some(folder_path))
}

#[cfg(feature = "web-ui")]
fn run_web_mode(
    folder_path: &Path,
    host: &str,
    port: u16,
    no_open: bool,
    test: bool,
) -> CliResult<Output> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| {
            CliError::new(
                "runtime-error",
                e.to_string(),
                "failed to start async runtime for UI",
            )
        })?;

    let folder_owned = folder_path.to_path_buf();
    let host_owned = host.to_string();

    rt.block_on(async move {
        let bind_addr_str = format!("{host_owned}:{port}");
        let listener = tokio::net::TcpListener::bind(&bind_addr_str)
            .await
            .map_err(|e| {
                CliError::new(
                    "bind-failed",
                    format!("failed to bind web UI listener to {bind_addr_str}: {e}"),
                    "check host/port parameters and permissions",
                )
            })?;

        let local_addr: SocketAddr = listener.local_addr().map_err(|e| {
            CliError::new("bind-failed", e.to_string(), "failed to read local address")
        })?;

        let token = generate_token();
        let url = format!(
            "http://{}:{}/?token={}",
            local_addr.ip(),
            local_addr.port(),
            token
        );

        let socket_path = ferry_ipc::paths::socket_path_for_dir(&folder_owned);
        let backend = ferry_daemon::ui::AutoBackend::new(socket_path).with_fallback(folder_owned);
        let server = DashboardServer::new(Arc::new(backend))
            .with_token(token.clone())
            .with_inactivity_timeout(INACTIVITY_TIMEOUT);

        if test {
            return Ok(Output::new(
                json!({
                    "command": "ui",
                    "frontend": "web",
                    "status": "ok",
                    "port": local_addr.port(),
                    "token": token,
                    "url": url,
                }),
                format!("Ferry UI listening on {url}\n"),
            ));
        }

        eprintln!("Ferry UI listening on {url}");
        eprintln!("One-time access token: {token}");
        eprintln!("Press Ctrl+C to stop the server.");

        if !no_open {
            open_browser(&url);
        }

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        // Inactivity monitor
        let monitor_server = server.clone();
        let monitor_shutdown = shutdown_tx.clone();
        let monitor_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                if monitor_server.idle_duration() >= INACTIVITY_TIMEOUT {
                    eprintln!("Ferry UI: inactive for 10 minutes, shutting down server.");
                    let _ = monitor_shutdown.send(true);
                    break;
                }
            }
        });

        let graceful_signal = async move {
            tokio::select! {
                _ = shutdown_rx.changed() => {},
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("\nReceived Ctrl+C, shutting down Ferry UI server.");
                }
            }
        };

        if let Err(e) = server
            .serve_with_graceful_shutdown(listener, graceful_signal)
            .await
        {
            monitor_handle.abort();
            return Err(CliError::new(
                "server-error",
                e.to_string(),
                "Axum server terminated with error",
            ));
        }

        monitor_handle.abort();

        Ok(Output::new(
            json!({
                "command": "ui",
                "frontend": "web",
                "status": "closed",
                "port": local_addr.port(),
            }),
            "Ferry UI server closed.\n",
        ))
    })
}

pub fn run(args: UiArgs) -> CliResult<Output> {
    let _folder_path = match args.folder {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    // 1. Explicit --gui flag
    if args.gui {
        #[cfg(feature = "gui")]
        {
            return run_gui_mode(&_folder_path, args.test);
        }
        #[cfg(not(feature = "gui"))]
        {
            return Err(CliError::new(
                "feature-disabled",
                "Feature 'gui' is not enabled in this build.",
                "Rebuild with: cargo build --features gui",
            ));
        }
    }

    // 2. Explicit --web flag
    if args.web {
        #[cfg(feature = "web-ui")]
        {
            return run_web_mode(&_folder_path, args.host, args.port, args.no_open, args.test);
        }
        #[cfg(not(feature = "web-ui"))]
        {
            return Err(CliError::new(
                "feature-disabled",
                "Feature 'web-ui' is not enabled in this build.",
                "Rebuild with: cargo build --features web-ui",
            ));
        }
    }

    // 3. Explicit --tui flag
    if args.tui {
        #[cfg(feature = "tui")]
        {
            return run_tui_mode(&_folder_path, args.test);
        }
        #[cfg(not(feature = "tui"))]
        {
            return Err(CliError::new(
                "feature-disabled",
                "Feature 'tui' is not enabled in this build.",
                "Rebuild with: cargo build --features tui",
            ));
        }
    }

    // 4. Default frontend selection (GUI -> Web -> TUI)
    #[cfg(feature = "gui")]
    {
        run_gui_mode(&_folder_path, args.test)
    }

    #[cfg(all(not(feature = "gui"), feature = "web-ui"))]
    {
        run_web_mode(&_folder_path, args.host, args.port, args.no_open, args.test)
    }

    #[cfg(all(not(feature = "gui"), not(feature = "web-ui"), feature = "tui"))]
    {
        run_tui_mode(&_folder_path, args.test)
    }

    #[cfg(all(not(feature = "gui"), not(feature = "web-ui"), not(feature = "tui")))]
    {
        Err(CliError::new(
            "feature-disabled",
            "No frontend feature ('gui', 'web-ui', or 'tui') is enabled in this build.",
            "Rebuild with: cargo build --features gui",
        ))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "web-ui")]
    use super::*;

    #[cfg(feature = "web-ui")]
    #[test]
    fn generated_tokens_are_32_hex_chars() {
        let tok1 = generate_token();
        let tok2 = generate_token();
        assert_eq!(tok1.len(), 32);
        assert_eq!(tok2.len(), 32);
        assert_ne!(tok1, tok2);
        assert!(tok1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[cfg(feature = "web-ui")]
    #[test]
    fn token_validation_is_constant_time_safe() {
        let expected = "abcdef0123456789abcdef0123456789";
        assert!(is_token_valid(expected, Some(expected)));
        assert!(!is_token_valid(expected, Some("wrong_token")));
        assert!(!is_token_valid(expected, None));
        assert!(!is_token_valid(expected, Some("")));
    }
}
