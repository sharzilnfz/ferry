//! Coded errors for folder bootstrap and the pairing ritual. Same shape and
//! codes as the CLI's error discipline (stable machine `code`, human
//! message, actionable hint) without any CLI coupling: frontends render
//! these however they like.

/// One actionable failure. `code` values are v0-frozen (see docs/cli-json.md).
#[derive(Debug)]
pub struct FolderError {
    /// Stable machine identifier. Never renamed.
    pub code: &'static str,
    /// What happened.
    pub message: String,
    /// What to try next.
    pub hint: String,
}

impl FolderError {
    pub fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        FolderError {
            code,
            message: message.into(),
            hint: hint.into(),
        }
    }
}

impl std::fmt::Display for FolderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (code={})\nhint: {}",
            self.message, self.code, self.hint
        )
    }
}

impl std::error::Error for FolderError {}

pub type FolderResult<T> = Result<T, FolderError>;

/// Shorthand for mapping foreign errors into coded ones at the boundary.
pub trait CodeInto<T> {
    fn code(self, code: &'static str, hint: impl Into<String>) -> FolderResult<T>;
}

impl<T, E: std::fmt::Display> CodeInto<T> for Result<T, E> {
    fn code(self, code: &'static str, hint: impl Into<String>) -> FolderResult<T> {
        self.map_err(|e| FolderError::new(code, e.to_string(), hint))
    }
}
