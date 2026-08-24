//! Typed errors for the pin subsystem. Every failure names its path (or
//! peer) and says why — corrupt state is loud, never silently ignored.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PinError {
    #[error("io failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("pin state at {path} is corrupt: {reason}")]
    Corrupt { path: PathBuf, reason: String },
    #[error("a pin is already active on this folder (started by pid {pid})")]
    PinActive { pid: u32 },
    #[error("invalid pin glob {line:?}: {reason}")]
    BadPattern { line: String, reason: String },
    #[error(
        "cannot split the plan safely under this pin: pinned path {pinned} sits inside \
         apply-path {other}; one half would move an ancestor of the other. Widen or narrow \
         --paths so pinned and unpinned changes do not nest."
    )]
    StructuralSplit { pinned: String, other: String },
    #[error("held manifest {manifest_id} for peer {peer} is missing from the store")]
    ManifestMissing { peer: String, manifest_id: String },
    #[error("held ledger at {path} is corrupt near line {line}: {reason}")]
    LedgerCorrupt {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error("reconcile failed during release: {0}")]
    Reconcile(#[from] ferry_sync_engine::ReconcileError),
    #[error("store: {0}")]
    Store(#[from] ferry_store::store::StoreError),
    #[error("manifest decode failed: {0}")]
    Manifest(#[from] ferry_store::manifest::ManifestError),
}

pub(crate) fn io_at(path: impl Into<PathBuf>, e: std::io::Error) -> PinError {
    PinError::Io {
        path: path.into(),
        source: e,
    }
}
