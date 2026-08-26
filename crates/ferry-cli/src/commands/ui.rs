//! Ephemeral on-demand Web UI dashboard (`ferry ui`).
//!
//! Launches a local web server on a random loopback port (or user-specified host/port),
//! generates a secure 32-character random hex token, enforces token auth on all API
//! endpoints, proxies queries to the daemon over IPC (with disk fallback), opens the
//! default browser, and shuts down automatically after 10 minutes of inactivity or on Ctrl+C.

pub mod disk;
pub mod error;
pub mod handlers;
pub mod ipc;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use subtle::ConstantTimeEq as _;

use crate::error::{CliError, CliResult};
use crate::out::Output;
pub use error::{ApiError, OpError};

/// 10 minutes of inactivity before automatic shutdown.
pub const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);

pub struct UiArgs<'a> {
    pub folder: Option<&'a Path>,
    pub host: &'a str,
    pub port: u16,
    pub no_open: bool,
    pub test: bool,
}

/// Shared server state.
#[derive(Clone)]
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

/// Generate a secure 32-character random hex token (16 random bytes).
#[must_use]
pub fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    ferry_store::format::hex(&bytes)
}

/// Constant-time token verification.
#[must_use]
pub fn is_token_valid(expected: &str, provided: Option<&str>) -> bool {
    let Some(prov) = provided else {
        return false;
    };
    if expected.len() != prov.len() {
        return false;
    }
    expected.as_bytes().ct_eq(prov.as_bytes()).into()
}

/// Extract authentication token from `Authorization: Bearer <token>` header or `?token=<token>` query param.
pub fn extract_token(req: &axum::extract::Request) -> Option<String> {
    if let Some(auth_val) = req.headers().get(axum::http::header::AUTHORIZATION) {
        if let Ok(s) = auth_val.to_str() {
            if let Some(tok) = s.strip_prefix("Bearer ") {
                let trimmed = tok.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                if k == "token" && !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }

    None
}

/// Middleware that records activity timestamp and enforces token authentication on `/api/*` endpoints.
pub async fn auth_and_activity_middleware(
    State(state): State<Arc<UiServerState>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    state.record_activity();

    let path = req.uri().path();
    if path == "/api" || path.starts_with("/api/") {
        let provided = extract_token(&req);
        if !is_token_valid(&state.token, provided.as_deref()) {
            return ApiError::forbidden(
                "access denied: invalid or missing token",
                "pass the token in the Authorization header (Bearer <token>) or ?token=<token> query parameter",
            )
            .into_response();
        }
    }

    next.run(req).await
}

/// Construct the Axum router for the web UI.
pub fn router(state: Arc<UiServerState>) -> Router {
    Router::new()
        .route("/", get(handlers::serve_index))
        .route("/index.html", get(handlers::serve_index))
        .route("/index", get(handlers::serve_index))
        .route("/style.css", get(handlers::serve_css))
        .route("/app.js", get(handlers::serve_js))
        .route("/api/status", get(handlers::api_status))
        .route("/api/conflicts", get(handlers::api_conflicts))
        .route("/api/share", post(handlers::api_share))
        .route("/api/pair/accept", post(handlers::api_pair_accept))
        .route("/api/pin/start", post(handlers::api_pin_start))
        .route("/api/pin/stop", post(handlers::api_pin_stop))
        .route("/api/pin/release", post(handlers::api_pin_release))
        .route("/api/events", get(handlers::api_events))
        .fallback(handlers::fallback)
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_and_activity_middleware,
        ))
        .with_state(state)
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

        let state = Arc::new(UiServerState::new(folder_path, token.clone()));
        let app = router(Arc::clone(&state));

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
        let monitor_state = Arc::clone(&state);
        let monitor_shutdown = shutdown_tx.clone();
        let monitor_handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                if monitor_state.idle_duration() >= INACTIVITY_TIMEOUT {
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

        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(graceful_signal)
            .await
        {
            monitor_handle.abort();
            return Err(CliError::new("server-error", e.to_string(), "Axum server terminated with error"));
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
