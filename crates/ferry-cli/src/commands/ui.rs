//! Ephemeral on-demand Web UI dashboard (`ferry ui`).
//!
//! Launches a local web server on a random loopback port (or user-specified host/port),
//! generates a secure 32-character random hex token, enforces token auth on all API
//! endpoints, proxies queries to the daemon over IPC (with disk fallback), opens the
//! default browser, and shuts down automatically after 10 minutes of inactivity or on Ctrl+C.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
pub use ferry_daemon::ui::{
    extract_token, generate_token, is_token_valid, ApiError, DashboardServer, IpcBackend, OpError,
};
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::out::Output;

/// 10 minutes of inactivity before automatic shutdown.
pub const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);

pub struct UiArgs<'a> {
    pub folder: Option<&'a Path>,
    pub host: &'a str,
    pub port: u16,
    pub no_open: bool,
    pub test: bool,
}

/// Shared server state preserved for backwards compatibility with tests and callers.
#[derive(Clone, Debug)]
pub struct UiServerState {
    pub folder: PathBuf,
    pub token: String,
    pub last_activity: Arc<std::sync::Mutex<Instant>>,
}

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

/// Construct the Axum router for the web UI backed by `DashboardServer` and `IpcBackend`.
pub fn router(state: Arc<UiServerState>) -> Router {
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&state.folder);
    let backend = IpcBackend::new(socket_path).with_fallback(state.folder.clone());
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

pub fn run(args: UiArgs) -> CliResult<Output> {
    let folder_path = match args.folder {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

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

    rt.block_on(async move {
        let bind_addr_str = format!("{}:{}", args.host, args.port);
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

        let socket_path = ferry_ipc::paths::socket_path_for_dir(&folder_path);
        let backend = IpcBackend::new(socket_path).with_fallback(folder_path);
        let server = DashboardServer::new(Arc::new(backend))
            .with_token(token.clone())
            .with_inactivity_timeout(INACTIVITY_TIMEOUT);

        if args.test {
            // Test mode: server bound successfully, verify and exit cleanly
            return Ok(Output::new(
                json!({
                    "command": "ui",
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

        if !args.no_open {
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

        if let Err(e) = server.serve_with_graceful_shutdown(listener, graceful_signal).await {
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
                "status": "closed",
                "port": local_addr.port(),
            }),
            "Ferry UI server closed.\n",
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_are_32_hex_chars() {
        let tok1 = generate_token();
        let tok2 = generate_token();
        assert_eq!(tok1.len(), 32);
        assert_eq!(tok2.len(), 32);
        assert_ne!(tok1, tok2);
        assert!(tok1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn token_validation_is_constant_time_safe() {
        let expected = "abcdef0123456789abcdef0123456789";
        assert!(is_token_valid(expected, Some(expected)));
        assert!(!is_token_valid(expected, Some("wrong_token")));
        assert!(!is_token_valid(expected, None));
        assert!(!is_token_valid(expected, Some("")));
    }
}
