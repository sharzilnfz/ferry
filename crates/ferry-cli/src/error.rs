//! Typed CLI errors. No anyhow: every error carries a stable machine
//! `code`, a human message, and a `hint` (what happened, what to try) —
//! the ticket's error discipline, enforced by the type.

/// One actionable failure. Rendered as
/// `error: <message> (code=<code>)\nhint: <hint>` for humans and as
/// `{\"error\":…,\"code\":…,\"hint\":…}` under `--json`.
#[derive(Debug)]
pub struct CliError {
    /// Stable machine identifier (see docs/cli-json.md). Never renamed.
    pub code: String,
    /// What happened.
    pub message: String,
    /// What to try next.
    pub hint: String,
    /// Optional structured detail (e.g. share's redacted findings), merged
    /// into the JSON error document under its own keys.
    pub detail: Option<serde_json::Value>,
}

impl CliError {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        CliError {
            code: code.into(),
            message: message.into(),
            hint: hint.into(),
            detail: None,
        }
    }

    /// Process exit status. Usage errors never get here (clap exits 2 on
    /// its own); everything else is a generic failure per the ticket
    /// ("all errors exit nonzero"), except the codes a CI script must be
    /// able to tell apart.
    pub fn exit_code(&self) -> u8 {
        match self.code.as_str() {
            "secrets-found" => 3,
            "daemon-stop-timeout" => 4,
            _ => 1,
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (code={})\nhint: {}",
            self.message, self.code, self.hint
        )
    }
}

impl std::error::Error for CliError {}

pub type CliResult<T> = Result<T, CliError>;

/// Shorthand for mapping foreign errors into coded ones at the boundary.
pub trait CodeInto<T> {
    fn code(self, code: &'static str, hint: impl Into<String>) -> CliResult<T>;
}

impl<T, E: std::fmt::Display> CodeInto<T> for Result<T, E> {
    fn code(self, code: &'static str, hint: impl Into<String>) -> CliResult<T> {
        self.map_err(|e| CliError::new(code, e.to_string(), hint))
    }
}
