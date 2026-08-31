use std::path::PathBuf;

use ferry_store::diff::{join_path, CompPath};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DivergeReason {
    ExpectedAbsent,

    ExpectedPresent,

    KindMismatch {
        expected: ferry_store::diff::EntryKind,
        found: ferry_store::diff::EntryKind,
    },
    ExecMismatch {
        expected: bool,
        found: bool,
    },
    SizeMismatch {
        expected: u64,
        found: u64,
    },
    TargetMismatch {
        expected: String,
        found: String,
    },

    ContentMismatch,

    NotInBase,
}

impl std::fmt::Display for DivergeReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DivergeReason::ExpectedAbsent => write!(f, "expected absent, but present on disk"),
            DivergeReason::ExpectedPresent => write!(f, "expected present, but missing on disk"),
            DivergeReason::KindMismatch { expected, found } => {
                write!(f, "kind mismatch: expected {expected:?}, found {found:?}")
            }
            DivergeReason::ExecMismatch { expected, found } => {
                write!(f, "exec bit mismatch: expected {expected}, found {found}")
            }
            DivergeReason::SizeMismatch { expected, found } => {
                write!(f, "size mismatch: expected {expected}, found {found}")
            }
            DivergeReason::TargetMismatch { expected, found } => {
                write!(
                    f,
                    "symlink target mismatch: expected {expected:?}, found {found:?}"
                )
            }
            DivergeReason::ContentMismatch => write!(f, "content differs from expected"),
            DivergeReason::NotInBase => {
                write!(
                    f,
                    "path is described by neither the change nor the expected manifest"
                )
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub path: CompPath,
    pub reason: DivergeReason,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", join_path(&self.path), self.reason)
    }
}

#[derive(Debug, Error)]
pub enum MaterializeError {
    #[error("store: {0}")]
    Store(#[from] ferry_store::store::StoreError),
    #[error("manifest decode failed: {0}")]
    Manifest(#[from] ferry_store::manifest::ManifestError),
    #[error("pin: {0}")]
    Pin(String),
    #[error("refusing stored name component {component:?} (traversal defense)")]
    BadComponent { component: String },
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "{path}: chunk #{index} failed verification after store read \
         (expected {expected}, got {found})"
    )]
    ChunkCorrupt {
        path: String,
        index: usize,
        expected: String,
        found: String,
    },
    #[error(
        "{path}: chunk #{index} failed re-verification in temp file before rename \
         (expected {expected}, got {found}); destination never touched"
    )]
    TempWriteVerifyFailed {
        path: String,
        index: usize,
        expected: String,
        found: String,
    },
    #[error("{path}: manifest declares size {declared} but chunks sum to {actual}")]
    SizeMismatch {
        path: String,
        declared: u64,
        actual: u64,
    },
    #[error("live tree diverged from the expected base state; nothing was modified:\n{}",
        paths.iter().map(|p| format!("  - {p}")).collect::<Vec<_>>().join("\n"))]
    Diverged { paths: Vec<Divergence> },
    #[error(
        "refusing stored name {path}: {component:?} is a reserved Windows device name \
         (CON, PRN, AUX, NUL, COM1-9, LPT1-9 with any extension); rename the entry on the \
         source device, e.g. add a prefix like `data-`"
    )]
    ReservedName { path: String, component: String },
    #[error("{path}: symlink target {target:?} — {reason}")]
    SymlinkRefused {
        path: String,
        target: String,
        reason: ferry_platform::LinkRefusal,
    },
    #[error(
        "{path}: directory symlinks/junctions are disabled on Windows because creating \
         them requires developer mode or admin rights; set FERRY_ALLOW_WINDOWS_DIR_LINKS=1 \
         to opt in (documented developer-mode flag)"
    )]
    WindowsDirLinkRefused { path: String },
    #[error(
        "manifest contains case-conflicting siblings under {parent}: {first:?} and \
         {second:?} cannot coexist on this filesystem; rename one of them (ferry never \
         picks silently)"
    )]
    CaseCollision {
        parent: String,
        first: String,
        second: String,
    },
    #[error(
        "ambiguous disk spelling in {parent}: {first:?} and {second:?} both normalize \
         to the same stored name; remove one of them on disk before syncing (ferry \
         never picks silently)"
    )]
    AmbiguousDiskSpelling {
        parent: String,
        first: String,
        second: String,
    },
}

pub(crate) fn io_at(path: impl Into<PathBuf>, e: std::io::Error) -> MaterializeError {
    MaterializeError::Io {
        path: path.into(),
        source: e,
    }
}
