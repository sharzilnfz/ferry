//! Scan-side errors. Mirrors the failure shapes of `ferry-store::snapshot`
//! (IO with the offending path, NFC sibling collisions) plus watcher-layer
//! failures owned by this crate.

use std::path::PathBuf;

use ferry_store::manifest::ManifestError;
use ferry_store::snapshot::SnapshotError;
use ferry_store::store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("io failed at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("siblings under {parent} collide after NFC normalization: {name}")]
    NameCollision { parent: String, name: String },
    #[error("store rejected a blob: {0}")]
    Store(#[from] StoreError),
    #[error("full snapshot failed: {0}")]
    Snapshot(#[from] SnapshotError),
    #[error("stored tree node failed validation: {0}")]
    Manifest(#[from] ManifestError),
    #[error("watcher setup failed: {0}")]
    Watch(String),
    #[error("scan worker stopped before completing")]
    Stopped,
}

impl From<std::io::Error> for ScanError {
    fn from(source: std::io::Error) -> Self {
        ScanError::Io {
            path: PathBuf::new(),
            source,
        }
    }
}
