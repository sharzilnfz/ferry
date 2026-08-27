use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

/// A handler failure shaped for the wire: `{"error": "...", "code": "...", "hint": "..."}`.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Value,
}

impl ApiError {
    #[must_use]
    pub fn new(
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

    #[must_use]
    pub fn internal(message: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            message.to_string(),
            "check the daemon's stderr log",
        )
    }

    #[must_use]
    pub fn forbidden(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message, hint)
    }

    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    #[must_use]
    pub fn body(&self) -> &Value {
        &self.body
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

pub use ferry_ipc::backend::OpError;

/// Spec status mapping: 400 validation/usage, 403 forbidden, 404 unknown path/resource,
/// 409 preconditions (`secrets-found`, `pin-active`,
/// `already-initialized`, ...), 500 internal, plus sentinel codes.
#[must_use]
pub fn status_for_code(code: &str) -> StatusCode {
    match code {
        "forbidden" => StatusCode::FORBIDDEN,
        "not-found" => StatusCode::NOT_FOUND,
        "warming-up" => StatusCode::SERVICE_UNAVAILABLE,
        "not-implemented" => StatusCode::NOT_IMPLEMENTED,
        "secrets-found" | "pin-active" | "already-initialized" | "pair-timeout" => {
            StatusCode::CONFLICT
        }
        "io"
        | "store"
        | "store-open"
        | "internal"
        | "config-corrupt"
        | "key-unwrap"
        | "identity-corrupt"
        | "pin-state-corrupt"
        | "held-ledger-corrupt"
        | "conflict-log"
        | "agreement-state" => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    }
}

impl From<OpError> for ApiError {
    fn from(e: OpError) -> Self {
        let mut base = Self::new(status_for_code(&e.code), &e.code, e.message, e.hint);
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
