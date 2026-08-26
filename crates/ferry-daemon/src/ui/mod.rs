//! Embedded HTTP dashboard: axum on tokio beside the sync engine.
//!
//! v0 stance (`.scratch/web-dashboard/spec.md`): loopback bind only, no
//! auth, JSON documents shaped exactly per `docs/cli-json.md`, and no GET
//! ever rescans or hashes the tree. `/api/status` reads only the engine's
//! cached folder pointers (short lock inside `EngineHandle`) plus cheap
//! `.ferry/` metadata files. `/api/events` (SSE) is deferred; it answers
//! 501 `not-implemented` and the UI degrades to polling.

mod actions;
mod status;
mod timefmt;

use std::future::IntoFuture as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::Value;

use ferry_crypto::identity::DeviceIdentity;
use ferry_sync::EngineHandle;
use ferry_store::format::hex as hex_str;

const INDEX_HTML: &[u8] = include_bytes!("../../assets/index.html");
const STYLE_CSS: &[u8] = include_bytes!("../../assets/style.css");
const APP_JS: &[u8] = include_bytes!("../../assets/app.js");

/// Everything a handler needs. Cheap to clone behind the router state Arc.
pub struct UiState {
    handle: EngineHandle,
    store_dir: PathBuf,
    tree_dir: PathBuf,
    folder_id: [u8; 16],
    device_hex: String,
    identity: DeviceIdentity,
}

impl UiState {
    pub fn new(
        handle: EngineHandle,
        store_dir: PathBuf,
        tree_dir: PathBuf,
        folder_id: [u8; 16],
        identity: DeviceIdentity,
    ) -> Self {
        Self {
            handle,
            device_hex: hex_str(identity.public()),
            identity,
            store_dir,
            tree_dir,
            folder_id,
        }
    }

    pub(crate) fn handle(&self) -> &EngineHandle {
        &self.handle
    }

    /// The daemon's folder root IS its `--tree`; `.ferry/` lives under the
    /// `--store` dir (engine layout).
    pub(crate) fn tree_dir(&self) -> &Path {
        &self.tree_dir
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.store_dir.join(".ferry")
    }

    pub(crate) fn folder_id(&self) -> [u8; 16] {
        self.folder_id
    }

    pub(crate) fn device_hex(&self) -> &str {
        &self.device_hex
    }

    pub(crate) fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }
}

/// Bind failure surfaces synchronously so `--ui` typos fail startup loudly;
/// the server itself runs detached and dies with the process.
pub fn spawn(addr: SocketAddr, state: Arc<UiState>) -> Result<(), String> {
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
            let app = router(state);
            let _ = rt.block_on(axum::serve(listener, app).into_future());
        })
        .map_err(|e| format!("ui thread: {e}"))?;
    Ok(())
}

fn router(state: Arc<UiState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/conflicts", get(api_conflicts))
        .route("/api/share", post(api_share))
        .route("/api/pair/accept", post(api_pair_accept))
        .route("/api/pin/start", post(api_pin_start))
        .route("/api/pin/stop", post(api_pin_stop))
        .route("/api/pin/release", post(api_pin_release))
        .route("/api/events", get(api_events))
        .fallback(fallback)
        .with_state(state)
}

async fn index() -> Response {
    html_response(INDEX_HTML)
}

#[allow(clippy::unused_async)]
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
        // SPA fallback: unknown non-API paths serve the index.
        None => html_response(INDEX_HTML),
    }
}

fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    match path {
        "/index.html" | "/index" => Some((INDEX_HTML, "text/html; charset=utf-8")),
        "/style.css" => Some((STYLE_CSS, "text/css; charset=utf-8")),
        "/app.js" => Some((APP_JS, "text/javascript; charset=utf-8")),
        _ => None,
    }
}

fn html_response(bytes: &'static [u8]) -> Response {
    ([("content-type", "text/html; charset=utf-8")], bytes).into_response()
}

// ---------------------------------------------------------------------------
// error plumbing
// ---------------------------------------------------------------------------

/// A handler failure already shaped for the wire: `{error, code, hint}`.
pub struct ApiError {
    status: StatusCode,
    body: Value,
}

impl ApiError {
    fn new(
        status: StatusCode,
        code: &str,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            status,
            body: serde_json::json!({
                "error": message.into(),
                "code": code,
                "hint": hint.into(),
            }),
        }
    }

    fn internal(message: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            message.to_string(),
            "check the daemon's stderr log",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// Failure from the operation layer; carries CLI-stable codes verbatim.
pub struct OpError {
    code: &'static str,
    message: String,
    hint: String,
    detail: Option<Value>,
}

impl OpError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
            detail: None,
        }
    }

    pub(crate) fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl From<std::io::Error> for OpError {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", e.to_string(), "check folder permissions and disk space")
    }
}

/// Spec status mapping: 400 validation/usage, 404 unknown path/resource,
/// 409 preconditions (`secrets-found`, `pin-active`,
/// `already-initialized`, ...), 500 internal, plus the two deferred
/// endpoints' sentinel codes.
fn status_for_code(code: &str) -> StatusCode {
    match code {
        "not-found" => StatusCode::NOT_FOUND,
        "warming-up" => StatusCode::SERVICE_UNAVAILABLE,
        "not-implemented" => StatusCode::NOT_IMPLEMENTED,
        "secrets-found" | "pin-active" | "already-initialized" | "pair-timeout" => {
            StatusCode::CONFLICT
        }
        "io" | "store" | "store-open" | "internal" | "config-corrupt" | "key-unwrap"
        | "identity-corrupt" | "pin-state-corrupt" | "held-ledger-corrupt" | "conflict-log"
        | "agreement-state" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    }
}

impl From<OpError> for ApiError {
    fn from(e: OpError) -> Self {
        let mut base = Self::new(status_for_code(e.code), e.code, e.message, e.hint);
        if let Some(Value::Object(map)) = e.detail {
            if let Value::Object(ref mut body) = base.body {
                for (k, v) in map {
                    body.insert(k, v);
                }
            }
        }
        base
    }
}

fn bad_body(rejection: JsonRejection) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "bad-request",
        rejection.body_text(),
        "send the JSON body documented in .scratch/web-dashboard/spec.md",
    )
}

async fn run_blocking<T, F>(state: &Arc<UiState>, op: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&UiState) -> Result<T, OpError> + Send + 'static,
{
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || op(&state))
        .await
        .map_err(|e| ApiError::internal(format!("ui worker: {e}")))?
        .map_err(ApiError::from)
}

// ---------------------------------------------------------------------------
// handlers
// ---------------------------------------------------------------------------

async fn api_status(State(state): State<Arc<UiState>>) -> Result<Json<Value>, ApiError> {
    let doc = run_blocking(&state, status::status_doc).await?;
    Ok(Json(doc))
}

async fn api_conflicts(State(state): State<Arc<UiState>>) -> Result<Json<Value>, ApiError> {
    let doc = run_blocking(&state, status::conflicts_doc).await?;
    Ok(Json(doc))
}

async fn api_share(
    State(state): State<Arc<UiState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let folder = body
        .get("folder")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let i_know = body.get("i_know").and_then(Value::as_bool).unwrap_or(false);
    let doc = run_blocking(&state, move |st| actions::share(st, folder.as_deref(), i_know)).await?;
    Ok(Json(doc))
}

async fn api_pair_accept(
    State(state): State<Arc<UiState>>,
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
    let payload_path = PathBuf::from(payload_path);
    let doc = run_blocking(&state, move |st| {
        actions::pair_accept(st, &payload_path, dir.as_deref())
    })
    .await?;
    Ok(Json(doc))
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

async fn api_pin_start(
    State(state): State<Arc<UiState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let paths = extract_paths(&body);
    let doc = run_blocking(&state, move |st| actions::pin_start(st, paths)).await?;
    Ok(Json(doc))
}

async fn api_pin_stop(
    State(state): State<Arc<UiState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let doc = run_blocking(&state, actions::pin_stop).await?;
    Ok(Json(doc))
}

async fn api_pin_release(
    State(state): State<Arc<UiState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let doc = run_blocking(&state, actions::pin_release).await?;
    Ok(Json(doc))
}

#[allow(clippy::unused_async)]
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

    #[test]
    fn deferred_and_gate_codes_map_to_their_spec_statuses() {
        assert_eq!(
            status_for_code("warming-up"),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            status_for_code("not-implemented"),
            StatusCode::NOT_IMPLEMENTED
        );
        assert_eq!(status_for_code("pin-active"), StatusCode::CONFLICT);
        assert_eq!(status_for_code("not-found"), StatusCode::NOT_FOUND);
        assert_eq!(status_for_code("bad-request"), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn assets_embed_with_mime_types() {
        let (html, mime) = asset("/index.html").expect("index");
        assert_eq!(mime, "text/html; charset=utf-8");
        assert!(!html.is_empty());
        assert_eq!(asset("/style.css").unwrap().1, "text/css; charset=utf-8");
        assert_eq!(
            asset("/app.js").unwrap().1,
            "text/javascript; charset=utf-8"
        );
        assert!(asset("/missing.png").is_none());
    }

    #[tokio::test]
    async fn fallback_returns_json_404_for_api_and_html_for_spa() {
        let res_api = fallback(Uri::from_static("/api/unknown")).await;
        assert_eq!(res_api.status(), StatusCode::NOT_FOUND);

        let res_api_bare = fallback(Uri::from_static("/api")).await;
        assert_eq!(res_api_bare.status(), StatusCode::NOT_FOUND);

        let res_spa = fallback(Uri::from_static("/some/page")).await;
        assert_eq!(res_spa.status(), StatusCode::OK);
    }
}
