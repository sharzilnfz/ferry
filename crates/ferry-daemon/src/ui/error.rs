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

/// Failure from the operation layer; carries CLI-stable codes verbatim.
#[derive(Debug)]
pub struct OpError {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
    pub detail: Option<Value>,
}

impl OpError {
    #[must_use]
    pub fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
            detail: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl From<std::io::Error> for OpError {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", e.to_string(), "check folder permissions and disk space")
    }
}

impl From<ferry_folder::FolderError> for OpError {
    fn from(e: ferry_folder::FolderError) -> Self {
        let code = match e.code {
            "not-found" => "not-found",
            "secrets-found" => "secrets-found",
            "already-initialized" => "already-initialized",
            "pair-timeout" => "pair-timeout",
            "io" => "io",
            "store" => "store",
            "store-open" => "store-open",
            "config-corrupt" => "config-corrupt",
            "key-unwrap" => "key-unwrap",
            "identity-corrupt" => "identity-corrupt",
            _ => "bad-request",
        };
        Self::new(code, e.message, e.hint)
    }
}

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
