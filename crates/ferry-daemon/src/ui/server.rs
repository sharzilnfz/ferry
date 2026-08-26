use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use super::backend::DashboardBackend;
use super::error::ApiError;

pub const INDEX_HTML: &[u8] = include_bytes!("../../assets/index.html");
pub const STYLE_CSS: &[u8] = include_bytes!("../../assets/style.css");
pub const APP_JS: &[u8] = include_bytes!("../../assets/app.js");

/// Unified Axum HTTP server for the Ferry web dashboard.
#[derive(Clone)]
pub struct DashboardServer {
    backend: Arc<dyn DashboardBackend>,
    token: Option<String>,
    inactivity_timeout: Option<Duration>,
    last_activity: Arc<std::sync::Mutex<Instant>>,
}

impl DashboardServer {
    /// Create a new dashboard server backed by a `DashboardBackend`.
    #[must_use]
    pub fn new(backend: Arc<dyn DashboardBackend>) -> Self {
        Self {
            backend,
            token: None,
            inactivity_timeout: None,
            last_activity: Arc::new(std::sync::Mutex::new(Instant::now())),
        }
    }

    /// Configure a one-time bearer token required for all `/api/*` endpoints.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Configure automatic server shutdown after a duration of inactivity.
    #[must_use]
    pub fn with_inactivity_timeout(mut self, timeout: Duration) -> Self {
        self.inactivity_timeout = Some(timeout);
        self
    }

    /// Return the configured token, if any.
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Return the reference to the underlying backend.
    #[must_use]
    pub fn backend(&self) -> &Arc<dyn DashboardBackend> {
        &self.backend
    }

    /// Record activity timestamp (called automatically by middleware on every request).
    pub fn record_activity(&self) {
        if let Ok(mut last) = self.last_activity.lock() {
            *last = Instant::now();
        }
    }

    /// How long the server has been idle since the last request.
    #[must_use]
    pub fn idle_duration(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    /// Construct the complete Axum router.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(serve_index))
            .route("/index.html", get(serve_index))
            .route("/index", get(serve_index))
            .route("/style.css", get(serve_css))
            .route("/app.js", get(serve_js))
            .route("/api/status", get(api_status))
            .route("/api/conflicts", get(api_conflicts))
            .route("/api/share", post(api_share))
            .route("/api/pair/accept", post(api_pair_accept))
            .route("/api/pin/start", post(api_pin_start))
            .route("/api/pin/stop", post(api_pin_stop))
            .route("/api/pin/release", post(api_pin_release))
            .route("/api/events", get(api_events))
            .fallback(fallback)
            .layer(axum::middleware::from_fn_with_state(
                self.clone(),
                auth_and_activity_middleware,
            ))
            .with_state(self.clone())
    }

    /// Bind and spawn the server on a dedicated thread with its own tokio runtime.
    pub fn spawn(self, addr: SocketAddr) -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| format!("ui runtime: {e}"))?;
        let listener = rt
            .block_on(async { tokio::net::TcpListener::bind(addr).await })
            .map_err(|e| format!("--ui bind {addr}: {e}"))?;
        println!("UI LISTENING {addr}");
        std::thread::Builder::new()
            .name("ferry-ui".to_string())
            .spawn(move || {
                let _ = rt.block_on(self.serve(listener));
            })
            .map_err(|e| format!("ui thread: {e}"))?;
        Ok(())
    }

    /// Serve on a pre-bound `TcpListener` until inactivity timeout or external shutdown.
    pub async fn serve(self, listener: tokio::net::TcpListener) -> Result<(), std::io::Error> {
        if let Some(timeout) = self.inactivity_timeout {
            let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
            let server = self.clone();
            let monitor_handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(1));
                loop {
                    interval.tick().await;
                    if server.idle_duration() >= timeout {
                        let _ = shutdown_tx.send(true);
                        break;
                    }
                }
            });
            let graceful = async move {
                let _ = shutdown_rx.changed().await;
            };
            let app = self.router();
            let res = axum::serve(listener, app)
                .with_graceful_shutdown(graceful)
                .await;
            monitor_handle.abort();
            res
        } else {
            let app = self.router();
            axum::serve(listener, app).await
        }
    }

    /// Serve on a pre-bound `TcpListener` with a caller-supplied graceful shutdown signal.
    pub async fn serve_with_graceful_shutdown<F>(
        self,
        listener: tokio::net::TcpListener,
        signal: F,
    ) -> Result<(), std::io::Error>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let app = self.router();
        axum::serve(listener, app)
            .with_graceful_shutdown(signal)
            .await
    }
}

// ---------------------------------------------------------------------------
// Token authentication & activity middleware
// ---------------------------------------------------------------------------

/// Generate a secure 32-character random hex token (16 random bytes).
#[must_use]
pub fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    ferry_store::format::hex(&bytes)
}

/// Constant-time token verification using `subtle::ConstantTimeEq`.
#[must_use]
pub fn is_token_valid(expected: &str, provided: Option<&str>) -> bool {
    let Some(prov) = provided else {
        return false;
    };
    if expected.len() != prov.len() {
        return false;
    }
    use subtle::ConstantTimeEq as _;
    expected.as_bytes().ct_eq(prov.as_bytes()).into()
}

/// Extract authentication token from `Authorization: Bearer <token>` header or `?token=<token>` query param.
#[must_use]
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

async fn auth_and_activity_middleware(
    State(server): State<DashboardServer>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    server.record_activity();

    if let Some(expected_token) = server.token() {
        let path = req.uri().path();
        if path == "/api" || path.starts_with("/api/") {
            let provided = extract_token(&req);
            if !is_token_valid(expected_token, provided.as_deref()) {
                return ApiError::forbidden(
                    "access denied: invalid or missing token",
                    "pass the token in the Authorization header (Bearer <token>) or ?token=<token> query parameter",
                )
                .into_response();
            }
        }
    }

    next.run(req).await
}

// ---------------------------------------------------------------------------
// Static SPA asset handlers
// ---------------------------------------------------------------------------

async fn serve_index() -> Response {
    ([("content-type", "text/html; charset=utf-8")], INDEX_HTML).into_response()
}

async fn serve_css() -> Response {
    ([("content-type", "text/css; charset=utf-8")], STYLE_CSS).into_response()
}

async fn serve_js() -> Response {
    ([("content-type", "text/javascript; charset=utf-8")], APP_JS).into_response()
}

#[must_use]
pub fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    match path {
        "/index.html" | "/index" | "/" => Some((INDEX_HTML, "text/html; charset=utf-8")),
        "/style.css" => Some((STYLE_CSS, "text/css; charset=utf-8")),
        "/app.js" => Some((APP_JS, "text/javascript; charset=utf-8")),
        _ => None,
    }
}

async fn fallback(uri: Uri) -> Response {
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "not-found",
            format!("no such endpoint {path}"),
            "see .scratch/web-dashboard/spec.md for the endpoint list",
        )
        .into_response();
    }
    match asset(path) {
        Some((bytes, mime)) => ([("content-type", mime)], bytes).into_response(),
        None => ([("content-type", "text/html; charset=utf-8")], INDEX_HTML).into_response(),
    }
}

// ---------------------------------------------------------------------------
// API route handlers
// ---------------------------------------------------------------------------

fn bad_body(rejection: JsonRejection) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "bad-request",
        rejection.body_text(),
        "send the valid JSON body",
    )
}

fn extract_paths(body: &Value) -> Option<Vec<String>> {
    body.get("paths").and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

async fn api_status(
    State(server): State<DashboardServer>,
) -> Result<Json<Value>, ApiError> {
    let doc = server.backend.get_status().await?;
    Ok(Json(doc))
}

async fn api_conflicts(
    State(server): State<DashboardServer>,
) -> Result<Json<Value>, ApiError> {
    let doc = server.backend.list_conflicts().await?;
    Ok(Json(doc))
}

async fn api_share(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let folder = body
        .get("folder")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let i_know = body.get("i_know").and_then(Value::as_bool).unwrap_or(false);
    let doc = server.backend.share(folder, i_know).await?;
    Ok(Json(doc))
}

async fn api_pair_accept(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let Some(payload_path) = body.get("payload_path").and_then(Value::as_str) else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "payload_path is required",
            "pass the pair-offer file written by the sharing device",
        ));
    };
    let dir = body.get("dir").and_then(Value::as_str).map(PathBuf::from);
    let doc = server
        .backend
        .pair_accept(PathBuf::from(payload_path), dir)
        .await?;
    Ok(Json(doc))
}

async fn api_pin_start(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let paths = extract_paths(&body);
    let doc = server.backend.start_pin(paths).await?;
    Ok(Json(doc))
}

async fn api_pin_stop(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let doc = server.backend.stop_pin().await?;
    Ok(Json(doc))
}

async fn api_pin_release(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let doc = server.backend.release_pin().await?;
    Ok(Json(doc))
}

async fn api_events() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not-implemented",
        "SSE streaming is deferred in this build",
        "poll /api/status instead (the bundled UI falls back to 2s polling)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::backend::BoxFuture;
    use crate::ui::OpError;
    use serde_json::json;

    struct FakeBackend {
        status_res: Result<Value, OpError>,
        conflicts_res: Result<Value, OpError>,
        pin_start_res: Result<Value, OpError>,
        pin_stop_res: Result<Value, OpError>,
        pin_release_res: Result<Value, OpError>,
        share_res: Result<Value, OpError>,
        pair_accept_res: Result<Value, OpError>,
    }

    impl Default for FakeBackend {
        fn default() -> Self {
            Self {
                status_res: Ok(json!({ "command": "status", "folder": "/test/folder" })),
                conflicts_res: Ok(json!({ "command": "conflicts", "entries": [] })),
                pin_start_res: Ok(json!({ "command": "pin", "action": "start" })),
                pin_stop_res: Ok(json!({ "command": "pin", "action": "stop" })),
                pin_release_res: Ok(json!({ "command": "pin", "action": "release" })),
                share_res: Ok(json!({ "command": "share", "status": "completed" })),
                pair_accept_res: Ok(json!({ "command": "pair", "status": "completed" })),
            }
        }
    }

    impl DashboardBackend for FakeBackend {
        fn get_status(&self) -> BoxFuture<'_, Result<Value, OpError>> {
            let res = self.status_res.as_ref().map(Clone::clone).map_err(|e| OpError::new(e.code, &e.message, &e.hint));
            Box::pin(async move { res })
        }

        fn list_conflicts(&self) -> BoxFuture<'_, Result<Value, OpError>> {
            let res = self.conflicts_res.as_ref().map(Clone::clone).map_err(|e| OpError::new(e.code, &e.message, &e.hint));
            Box::pin(async move { res })
        }

        fn start_pin(&self, _paths: Option<Vec<String>>) -> BoxFuture<'_, Result<Value, OpError>> {
            let res = self.pin_start_res.as_ref().map(Clone::clone).map_err(|e| OpError::new(e.code, &e.message, &e.hint));
            Box::pin(async move { res })
        }

        fn stop_pin(&self) -> BoxFuture<'_, Result<Value, OpError>> {
            let res = self.pin_stop_res.as_ref().map(Clone::clone).map_err(|e| OpError::new(e.code, &e.message, &e.hint));
            Box::pin(async move { res })
        }

        fn release_pin(&self) -> BoxFuture<'_, Result<Value, OpError>> {
            let res = self.pin_release_res.as_ref().map(Clone::clone).map_err(|e| OpError::new(e.code, &e.message, &e.hint));
            Box::pin(async move { res })
        }

        fn share(&self, _folder: Option<PathBuf>, _i_know: bool) -> BoxFuture<'_, Result<Value, OpError>> {
            let res = self.share_res.as_ref().map(Clone::clone).map_err(|e| OpError::new(e.code, &e.message, &e.hint));
            Box::pin(async move { res })
        }

        fn pair_accept(&self, _payload_path: PathBuf, _dir: Option<PathBuf>) -> BoxFuture<'_, Result<Value, OpError>> {
            let res = self.pair_accept_res.as_ref().map(Clone::clone).map_err(|e| OpError::new(e.code, &e.message, &e.hint));
            Box::pin(async move { res })
        }
    }

    async fn send_http(
        addr: SocketAddr,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> (u16, Value, String) {
        use std::fmt::Write as _;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect");

        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for (k, v) in headers {
            let _ = write!(req, "{k}: {v}\r\n");
        }
        if let Some(b) = body {
            let _ = write!(req, "Content-Length: {}\r\n", b.len());
            req.push_str("Content-Type: application/json\r\n\r\n");
            req.push_str(b);
        } else {
            req.push_str("\r\n");
        }

        stream.write_all(req.as_bytes()).await.expect("write");
        let mut res = Vec::new();
        stream.read_to_end(&mut res).await.expect("read");
        let res_str = String::from_utf8_lossy(&res).to_string();

        let status = res_str
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse::<u16>().ok())
            .unwrap_or(0);

        let body_str = if let Some(idx) = res_str.find("\r\n\r\n") {
            &res_str[idx + 4..]
        } else {
            ""
        };

        let json_val = serde_json::from_str(body_str).unwrap_or(Value::Null);
        (status, json_val, body_str.to_string())
    }

    #[tokio::test]
    async fn test_static_assets_serve_without_token() {
        let backend = Arc::new(FakeBackend::default());
        let server = DashboardServer::new(backend).with_token("test-token-1234567890123456");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server_task = tokio::spawn(async move {
            server.serve(listener).await.expect("serve");
        });

        let (status, _, body) = send_http(addr, "GET", "/", &[], None).await;
        assert_eq!(status, 200);
        assert!(body.contains("<!doctype html>"));

        let (status, _, body) = send_http(addr, "GET", "/index.html", &[], None).await;
        assert_eq!(status, 200);
        assert!(body.contains("<!doctype html>"));

        let (status, _, body) = send_http(addr, "GET", "/style.css", &[], None).await;
        assert_eq!(status, 200);
        assert!(body.contains("--bg"));

        let (status, _, body) = send_http(addr, "GET", "/app.js", &[], None).await;
        assert_eq!(status, 200);
        assert!(body.contains("loadStatus"));

        server_task.abort();
    }

    #[tokio::test]
    async fn test_token_auth_and_route_dispatch() {
        let token = generate_token();
        let backend = Arc::new(FakeBackend::default());
        let server = DashboardServer::new(backend).with_token(token.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server_task = tokio::spawn(async move {
            server.serve(listener).await.expect("serve");
        });

        // 1. Without token -> 403 Forbidden
        let (status, json, _) = send_http(addr, "GET", "/api/status", &[], None).await;
        assert_eq!(status, 403);
        assert_eq!(json["code"], "forbidden");

        // 2. With wrong token in query -> 403
        let (status, json, _) = send_http(addr, "GET", "/api/status?token=wrong", &[], None).await;
        assert_eq!(status, 403);
        assert_eq!(json["code"], "forbidden");

        // 3. With wrong token in header -> 403
        let (status, json, _) = send_http(
            addr,
            "GET",
            "/api/status",
            &[("Authorization", "Bearer wrong_token_value")],
            None,
        )
        .await;
        assert_eq!(status, 403);
        assert_eq!(json["code"], "forbidden");

        // 4. Valid token in query param -> 200 OK
        let (status, json, _) = send_http(
            addr,
            "GET",
            &format!("/api/status?token={token}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["command"], "status");
        assert_eq!(json["folder"], "/test/folder");

        // 5. Valid token in header -> 200 OK
        let auth_header = format!("Bearer {token}");
        let (status, json, _) = send_http(
            addr,
            "GET",
            "/api/conflicts",
            &[("Authorization", &auth_header)],
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["command"], "conflicts");

        // 6. POST endpoints with token
        let (status, json, _) = send_http(
            addr,
            "POST",
            &format!("/api/pin/start?token={token}"),
            &[],
            Some(r#"{"paths": ["src/**"]}"#),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["command"], "pin");
        assert_eq!(json["action"], "start");

        let (status, json, _) = send_http(
            addr,
            "POST",
            &format!("/api/pin/stop?token={token}"),
            &[],
            Some("{}"),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["command"], "pin");
        assert_eq!(json["action"], "stop");

        let (status, json, _) = send_http(
            addr,
            "POST",
            &format!("/api/pin/release?token={token}"),
            &[],
            Some("{}"),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["command"], "pin");
        assert_eq!(json["action"], "release");

        let (status, json, _) = send_http(
            addr,
            "POST",
            &format!("/api/share?token={token}"),
            &[],
            Some(r#"{"folder": "/my/folder", "i_know": true}"#),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["command"], "share");

        let (status, json, _) = send_http(
            addr,
            "POST",
            &format!("/api/pair/accept?token={token}"),
            &[],
            Some(r#"{"payload_path": "/tmp/offer.json"}"#),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(json["command"], "pair");

        server_task.abort();
    }

    #[tokio::test]
    async fn test_fallback_routes() {
        let token = generate_token();
        let backend = Arc::new(FakeBackend::default());
        let server = DashboardServer::new(backend).with_token(token.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server_task = tokio::spawn(async move {
            server.serve(listener).await.expect("serve");
        });

        // 1. Unknown API endpoint -> 404 with JSON ApiError
        let (status, json, _) = send_http(
            addr,
            "GET",
            &format!("/api/does_not_exist?token={token}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 404);
        assert_eq!(json["code"], "not-found");

        // 2. SPA fallback for non-API route -> 200 with HTML
        let (status, _, body) = send_http(addr, "GET", "/unknown/spa/path", &[], None).await;
        assert_eq!(status, 200);
        assert!(body.contains("<!doctype html>"));

        // 3. /api/events -> 501
        let (status, json, _) = send_http(
            addr,
            "GET",
            &format!("/api/events?token={token}"),
            &[],
            None,
        )
        .await;
        assert_eq!(status, 501);
        assert_eq!(json["code"], "not-implemented");

        server_task.abort();
    }

    #[test]
    fn test_token_utils() {
        let tok1 = generate_token();
        let tok2 = generate_token();
        assert_eq!(tok1.len(), 32);
        assert_eq!(tok2.len(), 32);
        assert_ne!(tok1, tok2);

        assert!(is_token_valid(&tok1, Some(&tok1)));
        assert!(!is_token_valid(&tok1, Some(&tok2)));
        assert!(!is_token_valid(&tok1, None));
        assert!(!is_token_valid(&tok1, Some("")));
    }
}
