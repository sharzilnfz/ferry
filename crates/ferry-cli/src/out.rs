//! Command outputs: one JSON document plus its human rendering. Commands
//! build BOTH (cheap); main picks by `--json`. Contract details and field
//! stability live in docs/cli-json.md.

/// What a command produced.
#[derive(Debug)]
pub struct Output {
    /// The stable machine document (`--json`). One value per invocation;
    /// progress chatter goes to stderr, never here.
    pub json: serde_json::Value,
    /// Human rendering: plain lines/tables, no color codes.
    pub human: String,
    /// Commands may request a nonzero exit without being errors
    /// (e.g. `ferry sync` timing out unconverged exits 1).
    pub exit_code: u8,
}

impl Output {
    pub fn new(json: serde_json::Value, human: impl Into<String>) -> Self {
        Output {
            json,
            human: human.into(),
            exit_code: 0,
        }
    }
}

/// Render a structured error for humans/stderr.
pub fn error_text(code: &str, message: &str, hint: &str) -> String {
    format!("error: {message} (code={code})\nhint: {hint}")
}

/// Render a structured error as JSON (one line).
pub fn error_json(code: &str, message: &str, hint: &str) -> String {
    serde_json::json!({
        "error": message,
        "code": code,
        "hint": hint,
    })
    .to_string()
}
