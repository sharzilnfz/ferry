#[derive(Debug)]
pub struct CliError {
    pub code: String,

    pub message: String,

    pub hint: String,

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

pub trait CodeInto<T> {
    fn code(self, code: &'static str, hint: impl Into<String>) -> CliResult<T>;
}

impl<T, E: std::fmt::Display> CodeInto<T> for Result<T, E> {
    fn code(self, code: &'static str, hint: impl Into<String>) -> CliResult<T> {
        self.map_err(|e| CliError::new(code, e.to_string(), hint))
    }
}
