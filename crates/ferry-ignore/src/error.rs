

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum IgnoreError {
    
    #[error("unknown preset id: {0} (known: claude, opencode)")]
    UnknownPreset(String),
    
    
    
    #[error("cannot read root ferry.ignore at {}: {source}", path.display())]
    ReadRootRule {
        path: PathBuf,
        source: std::io::Error,
    },
    
    #[error("preset parse error: {0}")]
    PresetJson(String),
    
    #[error("rule compile error: {0}")]
    Compile(String),
}
