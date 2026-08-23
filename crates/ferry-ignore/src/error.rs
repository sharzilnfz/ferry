//! Rule-file loading errors.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum IgnoreError {
    /// A preset id in the config is not a known built-in (typo guard).
    #[error("unknown preset id: {0} (known: claude, opencode)")]
    UnknownPreset(String),
    /// The root `ferry.ignore` exists but could not be read. Root rule files
    /// are explicit user artifacts, so construction fails loudly; nested
    /// unreadable files are treated as absent instead.
    #[error("cannot read root ferry.ignore at {}: {source}", path.display())]
    ReadRootRule {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A preset could not be deserialized from JSON.
    #[error("preset parse error: {0}")]
    PresetJson(String),
    /// Rule compilation failed (invalid glob the builder refused wholesale).
    #[error("rule compile error: {0}")]
    Compile(String),
}
