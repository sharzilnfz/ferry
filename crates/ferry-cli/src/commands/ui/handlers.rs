use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use ferry_ipc::protocol::{ClientCommand, DaemonMessage};

use super::error::ApiError;
use super::UiServerState;

const INDEX_HTML: &[u8] = include_bytes!("../../../../ferry-daemon/assets/index.html");
const STYLE_CSS: &[u8] = include_bytes!("../../../../ferry-daemon/assets/style.css");
const APP_JS: &[u8] = include_bytes!("../../../../ferry-daemon/assets/app.js");

pub async fn serve_index() -> Response {
    ([("content-type", "text/html; charset=utf-8")], INDEX_HTML).into_response()
}

pub async fn serve_css() -> Response {
    ([("content-type", "text/css; charset=utf-8")], STYLE_CSS).into_response()
}

pub async fn serve_js() -> Response {
    ([("content-type", "text/javascript; charset=utf-8")], APP_JS).into_response()
}

pub async fn fallback(uri: Uri) -> Response {
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

pub fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    match path {
        "/index.html" | "/index" | "/" => Some((INDEX_HTML, "text/html; charset=utf-8")),
        "/style.css" => Some((STYLE_CSS, "text/css; charset=utf-8")),
        "/app.js" => Some((APP_JS, "text/javascript; charset=utf-8")),
        _ => None,
    }
}

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

pub async fn api_status(
    State(state): State<Arc<UiServerState>>,
) -> Result<Json<Value>, ApiError> {
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&state.folder);
    if let Ok(snap) = super::ipc::query_daemon_status(&socket_path).await {
        return Ok(Json(super::ipc::snapshot_to_status_doc(&snap)));
    }

    // Disk fallback:
    let folder = state.folder.clone();
    let doc = tokio::task::spawn_blocking(move || super::disk::read_status_from_disk(&folder))
        .await
        .map_err(|e| ApiError::internal(format!("status worker: {e}")))?
        .map_err(ApiError::from)?;
    Ok(Json(doc))
}

pub async fn api_conflicts(
    State(state): State<Arc<UiServerState>>,
) -> Result<Json<Value>, ApiError> {
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&state.folder);
    if let Ok(entries) = super::ipc::query_daemon_conflicts(&socket_path).await {
        return Ok(Json(json!({
            "command": "conflicts",
            "folder": state.folder.display().to_string(),
            "entries": entries,
        })));
    }

    // Disk fallback:
    let folder = state.folder.clone();
    let doc = tokio::task::spawn_blocking(move || super::disk::read_conflicts_from_disk(&folder))
        .await
        .map_err(|e| ApiError::internal(format!("conflicts worker: {e}")))?
        .map_err(ApiError::from)?;
    Ok(Json(doc))
}

pub async fn api_share(
    State(state): State<Arc<UiServerState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let folder = body
        .get("folder")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let i_know = body.get("i_know").and_then(Value::as_bool).unwrap_or(false);
    let default_folder = state.folder.clone();

    let doc = tokio::task::spawn_blocking(move || {
        let target = folder.unwrap_or(default_folder);
        super::disk::share_folder(&target, i_know)
    })
    .await
    .map_err(|e| ApiError::internal(format!("share worker: {e}")))?
    .map_err(ApiError::from)?;
    Ok(Json(doc))
}

pub async fn api_pair_accept(
    State(_state): State<Arc<UiServerState>>,
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

    let doc = tokio::task::spawn_blocking(move || {
        super::disk::pair_accept_folder(&payload_path, dir.as_deref())
    })
    .await
    .map_err(|e| ApiError::internal(format!("pair worker: {e}")))?
    .map_err(ApiError::from)?;
    Ok(Json(doc))
}

pub async fn api_pin_start(
    State(state): State<Arc<UiServerState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(bad_body)?;
    let paths = extract_paths(&body);
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&state.folder);

    // Try IPC first:
    let cmd = ClientCommand::StartPin {
        paths: paths.clone().unwrap_or_default(),
    };
    if let Ok(resp) = super::ipc::query_daemon_command(&socket_path, cmd).await {
        match resp {
            DaemonMessage::Ack { command, message } => {
                return Ok(Json(json!({
                    "command": "pin",
                    "action": "start",
                    "folder": state.folder.display().to_string(),
                    "paths": paths.unwrap_or_else(|| vec!["*".to_string()]),
                    "status": command,
                    "message": message,
                })));
            }
            DaemonMessage::Error { code, message } => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    &code,
                    message,
                    "stop or release existing pin first",
                ));
            }
            _ => {}
        }
    }

    // Disk fallback:
    let folder = state.folder.clone();
    let doc = tokio::task::spawn_blocking(move || super::disk::pin_start_disk(&folder, paths))
        .await
        .map_err(|e| ApiError::internal(format!("pin start worker: {e}")))?
        .map_err(ApiError::from)?;
    Ok(Json(doc))
}

pub async fn api_pin_stop(
    State(state): State<Arc<UiServerState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let folder = state.folder.clone();
    let doc = tokio::task::spawn_blocking(move || super::disk::pin_stop_disk(&folder))
        .await
        .map_err(|e| ApiError::internal(format!("pin stop worker: {e}")))?
        .map_err(ApiError::from)?;
    Ok(Json(doc))
}

pub async fn api_pin_release(
    State(state): State<Arc<UiServerState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(_) = payload.map_err(bad_body)?;
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&state.folder);

    // Try IPC first:
    let cmd = ClientCommand::ReleasePin;
    if let Ok(resp) = super::ipc::query_daemon_command(&socket_path, cmd).await {
        match resp {
            DaemonMessage::Ack { command, message } => {
                return Ok(Json(json!({
                    "command": "pin",
                    "action": "release",
                    "folder": state.folder.display().to_string(),
                    "status": command,
                    "message": message,
                })));
            }
            DaemonMessage::Error { code, message } => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    &code,
                    message,
                    "reconciliation error",
                ));
            }
            _ => {}
        }
    }

    // Disk fallback:
    let folder = state.folder.clone();
    let doc = tokio::task::spawn_blocking(move || super::disk::pin_release_disk(&folder))
        .await
        .map_err(|e| ApiError::internal(format!("pin release worker: {e}")))?
        .map_err(ApiError::from)?;
    Ok(Json(doc))
}

#[allow(clippy::unused_async)]
pub async fn api_events() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not-implemented",
        "SSE streaming is deferred in this build",
        "poll /api/status instead (the bundled UI falls back to 2s polling)",
    )
}
