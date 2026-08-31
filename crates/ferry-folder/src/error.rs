




use std::path::Path;


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderError {
    
    pub code: &'static str,
    
    pub message: String,
    
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

    
    pub fn not_initialized(path: impl AsRef<Path>) -> Self {
        FolderError::new(
            Self::NOT_INITIALIZED_CODE,
            format!(
                "{} is not an initialized Ferry folder",
                path.as_ref().display()
            ),
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
        assert_eq!(
            err.message,
            "/tmp/test_dir is not an initialized Ferry folder"
        );
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
        assert_eq!(
            err2.message,
            "/another/path is not an initialized Ferry folder"
        );
    }
}
