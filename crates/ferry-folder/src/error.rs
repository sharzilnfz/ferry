//! Coded errors for folder bootstrap and the pairing ritual. Same shape and
//! codes as the CLI's error discipline (stable machine `code`, human
//! message, actionable hint) without any CLI coupling: frontends render
//! these however they like.

use std::path::Path;

/// One actionable failure. `code` values are v0-frozen (see docs/cli-json.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderError {
    /// Stable machine identifier. Never renamed.
    pub code: &'static str,
    /// What happened.
    pub message: String,
    /// What to try next.
    pub hint: String,
}

impl FolderError {
    pub const NOT_INITIALIZED_CODE: &'static str = "not-initialized";
    pub const NOT_INITIALIZED_HINT: &'static str =
        "run 'ferry init' or 'ferry pair' before syncing this folder";

    pub fn new(code: &'static str, message: impl Into<String>, hint: impl Into<String>) -> Self {
        FolderError {
            code,
            message: message.into(),
            hint: hint.into(),
        }
    }

    /// Construct a canonical error for an uninitialized Ferry folder.
    pub fn not_initialized(path: impl AsRef<Path>) -> Self {
        FolderError::new(
            Self::NOT_INITIALIZED_CODE,
            format!("{} is not an initialized Ferry folder", path.as_ref().display()),
            Self::NOT_INITIALIZED_HINT,
        )
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_not_initialized_error() {
        let err = FolderError::not_initialized("/tmp/test_dir");
        assert_eq!(err.code, FolderError::NOT_INITIALIZED_CODE);
        assert_eq!(err.code, "not-initialized");
        assert_eq!(err.message, "/tmp/test_dir is not an initialized Ferry folder");
        assert_eq!(
            err.hint,
            "run 'ferry init' or 'ferry pair' before syncing this folder"
        );
        assert_eq!(err.hint, FolderError::NOT_INITIALIZED_HINT);
        assert_eq!(
            err.to_string(),
            "/tmp/test_dir is not an initialized Ferry folder (code=not-initialized)\nhint: run 'ferry init' or 'ferry pair' before syncing this folder"
        );

        let path = PathBuf::from("/another/path");
        let err2 = FolderError::not_initialized(&path);
        assert_eq!(err2.code, FolderError::NOT_INITIALIZED_CODE);
        assert_eq!(err2.hint, FolderError::NOT_INITIALIZED_HINT);
        assert_eq!(err2.message, "/another/path is not an initialized Ferry folder");
    }
}
