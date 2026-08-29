use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::middleware::Next;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use ferry_ipc::backend::{UiBackend, UiEvent};
use serde_json::{json, Value};
use std::convert::Infallible;

use super::backend::snapshot_to_status_doc;
use super::error::ApiError;

pub const INDEX_HTML: &[u8] = include_bytes!("../../assets/index.html");
pub const STYLE_CSS: &[u8] = include_bytes!("../../assets/style.css");
pub const APP_JS: &[u8] = include_bytes!("../../assets/app.js");

/// Unified Axum HTTP server for the Ferry web dashboard.
#[derive(Clone)]
pub struct DashboardServer {
    backend: Arc<dyn UiBackend>,
    token: Option<String>,
    inactivity_timeout: Option<Duration>,
    last_activity: Arc<std::sync::Mutex<Instant>>,
}

impl DashboardServer {
    /// Create a new dashboard server backed by a `UiBackend`.
    #[must_use]
    pub fn new(backend: Arc<dyn UiBackend>) -> Self {
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
    pub fn backend(&self) -> &Arc<dyn UiBackend> {
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
            .route("/api/share/status", get(api_share_status))
            .route("/api/pair/accept", post(api_pair_accept))
            .route("/api/pair/create", post(api_pair_create))
            .route("/api/pair/join", post(api_pair_join))
            .route("/api/pin/start", post(api_pin_start))
            .route("/api/pin/stop", post(api_pin_stop))
            .route("/api/pin/release", post(api_pin_release))
            .route("/api/fs/ls", get(api_fs_ls))
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

    if let Some(ref expected_token) = server.token {
        let path = req.uri().path();
        if path.starts_with("/api/") {
            let provided = extract_token(&req);
            if !is_token_valid(expected_token, provided.as_deref()) {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    "invalid or missing authentication token",
                    "provide valid token via Authorization: Bearer header or ?token query param",
                )
                .into_response();
            }
        }
    }

    next.run(req).await
}

// ---------------------------------------------------------------------------
// Static asset handlers
// ---------------------------------------------------------------------------

#[must_use]
pub fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    match path {
        "/" | "/index.html" | "/index" => Some((INDEX_HTML, "text/html; charset=utf-8")),
        "/style.css" => Some((STYLE_CSS, "text/css; charset=utf-8")),
        "/app.js" => Some((APP_JS, "text/javascript; charset=utf-8")),
        _ => None,
    }
}

async fn serve_index() -> Response {
    ([("content-type", "text/html; charset=utf-8")], INDEX_HTML).into_response()
}

async fn serve_css() -> Response {
    ([("content-type", "text/css; charset=utf-8")], STYLE_CSS).into_response()
}

async fn serve_js() -> Response {
    ([("content-type", "text/javascript; charset=utf-8")], APP_JS).into_response()
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

async fn api_status(State(server): State<DashboardServer>) -> Result<Json<Value>, ApiError> {
    let snap = server.backend.get_status().await?;
    Ok(Json(snapshot_to_status_doc(&snap)))
}

async fn api_conflicts(State(server): State<DashboardServer>) -> Result<Json<Value>, ApiError> {
    let entries = server.backend.list_conflicts().await?;
    let folder = server
        .backend
        .get_status()
        .await
        .map(|s| s.folder)
        .unwrap_or_default();
    Ok(Json(json!({
        "command": "conflicts",
        "folder": folder,
        "entries": entries,
    })))
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
    let offer = server.backend.share_initiate(folder, i_know).await?;
    Ok(Json(json!({
        "command": "share",
        "role": "initiate",
        "status": "pending",
        "folder": offer.folder,
        "short_code": offer.token,
        "offer_file": offer.payload_path.map(|p| p.display().to_string()),
        "warnings": offer.secret_warnings,
    })))
}

async fn api_share_status(
    State(server): State<DashboardServer>,
    req: axum::extract::Request,
) -> Result<Json<Value>, ApiError> {
    let mut folder = None;
    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            let mut parts = pair.splitn(2, '=');
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                if k == "folder" && !v.is_empty() {
                    folder = Some(PathBuf::from(v));
                }
            }
        }
    }
    let st = server.backend.share_status(folder).await?;
    let short_code = st.offer.as_ref().map(|o| o.token.clone());
    let offer_file = st
        .offer
        .as_ref()
        .and_then(|o| o.payload_path.as_ref().map(|p| p.display().to_string()));
    let mut doc = json!({
        "command": "share",
        "role": "initiate",
        "status": st.status,
        "folder": st.folder,
        "short_code": short_code,
        "offer_file": offer_file,
    });
    if let Some(peer) = st.peer_device_id {
        doc["peer_device_id"] = json!(peer);
    }
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
    let res = server
        .backend
        .pair_accept(PathBuf::from(payload_path), dir)
        .await?;
    Ok(Json(json!({
        "command": "pair",
        "role": "accept",
        "status": res.status,
        "folder": res.folder_path.display().to_string(),
        "folder_id": res.folder_id,
        "device_id": res.device_id,
    })))
}

async fn api_pair_create(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let folder_id = body
        .get("folder_id")
        .or_else(|| body.get("folderId"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad-request",
                "folder_id is required",
                "pass the folder_id to share",
            )
        })?;
    let req = ferry_ipc::pairing::CreatePairingRequest::new(folder_id.to_string());
    let resp = server.backend.create_pairing_session(req).await?;
    Ok(Json(json!({
        "command": "pair",
        "role": "create",
        "status": "pending",
        "code": resp.code,
        "expires_at": resp.expires_at,
    })))
}

async fn api_pair_join(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let code = body.get("code").and_then(Value::as_str).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad-request",
            "code is required",
            "pass the 6-character pairing code",
        )
    })?;
    let target_dir = body
        .get("target_dir")
        .or_else(|| body.get("targetDir"))
        .or_else(|| body.get("dir"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad-request",
                "target_dir is required",
                "pass the directory to create the synced folder in",
            )
        })?;
    let req = ferry_ipc::pairing::JoinPairingRequest::new(code.to_string(), target_dir);
    let res = server.backend.join_pairing_session(req).await?;
    Ok(Json(json!({
        "command": "pair",
        "role": "join",
        "status": res.status,
        "folder": res.folder_path.display().to_string(),
        "folder_id": res.folder_id,
        "device_id": res.device_id,
    })))
}

async fn api_pin_start(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let paths = extract_paths(&body).unwrap_or_default();
    let res = server.backend.start_pin(paths, None).await?;
    Ok(Json(json!({
        "command": "pin",
        "action": "start",
        "folder": res.folder,
        "paths": res.paths,
        "status": res.status,
        "message": res.message,
    })))
}

async fn api_pin_stop(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let res = server.backend.stop_pin().await?;
    Ok(Json(json!({
        "command": "pin",
        "action": "stop",
        "folder": res.folder,
        "status": res.status,
        "message": res.message,
    })))
}

async fn api_pin_release(
    State(server): State<DashboardServer>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let res = server.backend.release_pin().await?;
    Ok(Json(json!({
        "command": "pin",
        "action": "release",
        "folder": res.folder,
        "released_changes": res.released_changes,
        "status": res.status,
        "message": res.message,
    })))
}

fn percent_decode_query_value(input: &str) -> Result<String, ApiError> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hi = chars.next().ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad-path",
                    "invalid percent encoding in path",
                    "use absolute path",
                )
            })?;
            let lo = chars.next().ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad-path",
                    "invalid percent encoding in path",
                    "use absolute path",
                )
            })?;
            let hex = format!("{hi}{lo}");
            let byte = u8::from_str_radix(&hex, 16).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad-path",
                    "invalid percent encoding in path",
                    "use absolute path",
                )
            })?;
            if byte == 0 {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad-path",
                    "path contains null byte",
                    "use absolute path",
                ));
            }
            out.push(byte as char);
        } else if c == '+' {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    Ok(out)
}

async fn api_fs_ls(
    State(server): State<DashboardServer>,
    req: axum::extract::Request,
) -> Result<Json<Value>, ApiError> {
    let raw_query = req.uri().query().unwrap_or("").to_string();
    if raw_query.to_ascii_lowercase().contains("%00") {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad-path",
            "path contains null byte",
            "use absolute path",
        ));
    }
    let mut decoded_path: Option<String> = None;
    let mut found_path = false;
    for pair in raw_query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut kv = pair.splitn(2, '=');
        let k_raw = kv.next().unwrap_or("");
        let v_raw = kv.next().unwrap_or("");
        let k_dec = percent_decode_query_value(k_raw).unwrap_or(k_raw.to_string());
        if k_dec == "path" {
            found_path = true;
            let v_dec = percent_decode_query_value(v_raw)?;
            if v_dec.contains('\0') {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad-path",
                    "path contains null byte",
                    "use absolute path",
                ));
            }
            if v_raw.to_ascii_lowercase().contains("%2e") && v_dec.contains("..") {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "path-traversal",
                    format!("path {v_dec} escapes allowed root"),
                    "path escapes allowed root",
                ));
            }
            if v_dec.is_empty() {
                decoded_path = None;
            } else {
                decoded_path = Some(v_dec);
            }
            break;
        }
    }
    let path_opt = if found_path {
        decoded_path.map(PathBuf::from)
    } else {
        None
    };
    if let Some(ref p) = path_opt {
        let s = p.to_string_lossy().to_string();
        if s.contains('\0') {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad-path",
                "path contains null byte",
                "use absolute path",
            ));
        }
        if s.contains("//") {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad-path",
                format!("path {s} contains //"),
                "use single slashes",
            ));
        }
    }
    let resp = server.backend.list_directory(path_opt).await.map_err(|e| {
        let status = match e.code.as_str() {
            "path-traversal" => StatusCode::FORBIDDEN,
            "bad-path" => StatusCode::BAD_REQUEST,
            _ => super::error::status_for_code(&e.code),
        };
        ApiError::new(status, &e.code, e.message, e.hint)
    })?;
    Ok(Json(json!({
        "entries": resp.entries,
        "absolute_path": resp.absolute_path.display().to_string(),
    })))
}

async fn api_events(
    State(server): State<DashboardServer>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>> + Send + 'static> {
    let (tx, rx) = tokio::sync::mpsc::channel(16);

    tokio::spawn(async move {
        // 1. Send initial status snapshot on connection
        if let Ok(snap) = server.backend.get_status().await {
            let doc = snapshot_to_status_doc(&snap);
            let s = serde_json::to_string(&doc).unwrap_or_default();
            if tx
                .send(Ok(Event::default().event("state").data(s)))
                .await
                .is_err()
            {
                return;
            }
        }

        // 2. Stream events from backend push stream with zero polling (0.0% idle CPU)
        if let Ok(mut stream) = server.backend.subscribe_events().await {
            while let Ok(event) = stream.recv().await {
                if tx.is_closed() {
                    break;
                }
                let sse_event = match event {
                    UiEvent::State(snap) => {
                        let doc = snapshot_to_status_doc(&snap);
                        Event::default()
                            .event("state")
                            .data(serde_json::to_string(&doc).unwrap_or_default())
                    }
                    UiEvent::ConflictRecorded {
                        path,
                        conflict_path,
                        timestamp,
                        quarantined_as,
                    } => Event::default().event("conflict").data(
                        json!({
                            "path": path,
                            "conflict_path": conflict_path,
                            "timestamp": timestamp,
                            "quarantined_as": quarantined_as,
                        })
                        .to_string(),
                    ),
                    UiEvent::TransferProgress {
                        bytes_transferred,
                        total_bytes,
                        current_path,
                        chunks_transferred,
                        total_chunks,
                        peer_device_id,
                        direction,
                    } => Event::default().event("transfer").data(
                        json!({
                            "bytes_transferred": bytes_transferred,
                            "total_bytes": total_bytes,
                            "current_path": current_path,
                            "chunks_transferred": chunks_transferred,
                            "total_chunks": total_chunks,
                            "peer_device_id": peer_device_id,
                            "direction": direction,
                        })
                        .to_string(),
                    ),
                    UiEvent::StateChanged {
                        state,
                        manifest_id,
                        agreed_id,
                        pending_changes,
                        stats,
                    } => Event::default().event("state_changed").data(
                        json!({
                            "state": state,
                            "manifest_id": manifest_id,
                            "agreed_id": agreed_id,
                            "pending_changes": pending_changes,
                            "stats": stats,
                        })
                        .to_string(),
                    ),
                    UiEvent::Error { code, message } => Event::default().event("error").data(
                        json!({
                            "code": code,
                            "message": message,
                        })
                        .to_string(),
                    ),
                };
                if tx.send(Ok(sse_event)).await.is_err() {
                    break;
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_ipc::FakeBackend;

    async fn send_http(
        addr: SocketAddr,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> (u16, Value, String) {
        use std::fmt::Write as _;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for (k, v) in headers {
            let _ = writeln!(req, "{k}: {v}\r\n");
        }
        if let Some(b) = body {
            let _ = writeln!(req, "Content-Length: {}\r\n\r\n{b}", b.len());
        } else {
            req.push_str("\r\n");
        }

        stream.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.unwrap();
        let response_str = String::from_utf8_lossy(&buf).to_string();

        let mut status_code = 0;
        let mut body_part = "";
        if let Some(first_line) = response_str.lines().next() {
            let parts: Vec<&str> = first_line.split_whitespace().collect();
            if parts.len() >= 2 {
                status_code = parts[1].parse().unwrap_or(0);
            }
        }
        if let Some(idx) = response_str.find("\r\n\r\n") {
            body_part = &response_str[idx + 4..];
        }

        let json_body: Value = serde_json::from_str(body_part).unwrap_or(Value::Null);
        (status_code, json_body, body_part.to_string())
    }

    #[tokio::test]
    async fn server_serves_static_assets() {
        let backend = Arc::new(FakeBackend::new());
        let server = DashboardServer::new(backend);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            server.serve(listener).await.unwrap();
        });

        let (status, _, body) = send_http(addr, "GET", "/index.html", &[], None).await;
        assert_eq!(status, 200);
        assert!(body.to_ascii_lowercase().contains("<!doctype html>"));

        let (status, _, body) = send_http(addr, "GET", "/style.css", &[], None).await;
        assert_eq!(status, 200);
        assert!(body.contains("border-box") || body.contains("background"));

        let (status, _, _body) = send_http(addr, "GET", "/app.js", &[], None).await;
        assert_eq!(status, 200);

        handle.abort();
    }

    #[tokio::test]
    async fn server_enforces_token_auth_on_api() {
        let backend = Arc::new(FakeBackend::new());
        let token = "secret_test_token_123456789012345";
        let server = DashboardServer::new(backend).with_token(token);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            server.serve(listener).await.unwrap();
        });

        // 1. Missing token -> 403
        let (status, body, _) = send_http(addr, "GET", "/api/status", &[], None).await;
        assert_eq!(status, 403);
        assert_eq!(body["code"], "forbidden");

        // 2. Wrong token -> 403
        let (status, body, _) = send_http(
            addr,
            "GET",
            "/api/status",
            &[("Authorization", "Bearer wrong_token_00000000000000000000")],
            None,
        )
        .await;
        assert_eq!(status, 403);
        assert_eq!(body["code"], "forbidden");

        // 3. Valid Bearer token header -> 200
        let auth_hdr = format!("Bearer {token}");
        let (status, body, _) = send_http(
            addr,
            "GET",
            "/api/status",
            &[("Authorization", &auth_hdr)],
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(body["command"], "status");

        // 4. Valid query param token -> 200
        let query_path = format!("/api/status?token={token}");
        let (status, body, _) = send_http(addr, "GET", &query_path, &[], None).await;
        assert_eq!(status, 200);
        assert_eq!(body["command"], "status");

        handle.abort();
    }
}
