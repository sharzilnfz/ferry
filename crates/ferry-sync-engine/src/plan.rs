//! The action plan: everything a caller needs to execute one reconcile
//! cycle. Pure data; [`crate::reconcile`] builds it, [`crate::execute`]
//! runs it.
//!
//! Ordering contract: quarantine ops execute BEFORE materialize ops (a
//! local loser's bytes must be saved off the live path before the winner
//! overwrites it), and conflict entries are appended to the report only
//! after both succeed.

use ferry_store::diff::{CompPath, EntryState};
use ferry_store::format::BlobId;
use ferry_store::manifest::RootManifest;

/// Which side of an exchange a decision came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

/// What made a divergent path a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides changed the same path differently from base.
    BothChanged,
    /// One side deleted, the other edited; the edit resurrects.
    DeleteVsEdit,
    /// No base existed and the sides added different content.
    AddVsAdd,
}

/// One planned transition for one path: from `base` (None = absent in the
/// ancestor) to `result` (None = delete). The executor folds these into a
/// single change set for the applier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializeOp {
    pub path: CompPath,
    pub base: Option<EntryState>,
    pub result: Option<EntryState>,
}

/// Where a loser copy's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoserContent {
    /// The local live FILE is the loser: read its bytes before any
    /// overwrite, verifying them region-by-region against the chunk list
    /// the local manifest declares. A mismatch surfaces as `Diverged`
    /// before anything is written anywhere.
    LiveLocal { expected_chunks: Vec<(BlobId, u64)> },
    /// The local live SYMLINK is the loser: recreate it from the target the
    /// local manifest declares after checking the live link still matches.
    LiveLocalSymlink { expected_target: String },
    /// The remote side is the loser: reassemble from blobs already in the
    /// store (fetched with the plan's `fetch` list when local lacks them).
    FromStore {
        kind: ferry_store::diff::EntryKind,
        exec: bool,
        mtime_sec: i64,
        mtime_nsec: u32,
        chunks: Vec<(BlobId, u64)>,
        target: Option<String>,
    },
}

/// Save one losing version as `path.ferry-conflict.<loser-device>-<ts>`
/// next to the winner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuarantineOp {
    pub path: CompPath,
    /// The loser device whose short id names the file.
    pub loser_device: [u8; 32],
    /// The loser entry's mtime (names the file AND stamps the copy).
    pub loser_mtime_sec: i64,
    pub loser_mtime_nsec: u32,
    /// Exec bit of the loser entry (files only).
    pub exec: bool,
    pub content: LoserContent,
}

/// One conflict destined for the structured report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedConflict {
    pub path: CompPath,
    pub kind: ConflictKind,
    pub winner: Side,
    pub loser: Side,
    /// Full device ids for the report line.
    pub winner_device: [u8; 32],
    pub loser_device: [u8; 32],
    /// Winner entry mtime; always present (a resurrection winner is an
    /// existing entry).
    pub winner_mtime_sec: i64,
    pub winner_mtime_nsec: u32,
    /// Loser mtime; None means the loser is a deletion.
    pub loser_mtime_sec: Option<i64>,
    pub loser_mtime_nsec: Option<u32>,
    /// Set by the executor once the quarantine name is resolved.
    pub quarantined_as: Option<String>,
}

/// Everything one reconcile cycle decided.
#[derive(Clone, Debug, Default)]
pub struct ActionPlan {
    /// Ordered per-path transitions toward the merged result, executed via
    /// the ferry-materialize applier guarded against the LOCAL manifest.
    pub materialize: Vec<MaterializeOp>,
    /// Loser copies to write before any overwrite happens.
    pub quarantine: Vec<QuarantineOp>,
    /// Data chunks this device must SEND so the peer converges: chunks the
    /// merged result references that the remote manifest does not.
    pub send: Vec<(BlobId, u64)>,
    /// Data chunks to FETCH before executing: chunks the plan references
    /// that the local store may lack (remote-origin winners). Computed as
    /// "not referenced anywhere in the local manifest"; fetching these from
    /// the peer first makes execution self-sufficient.
    pub fetch: Vec<(BlobId, u64)>,
    pub conflicts: Vec<PlannedConflict>,
    /// The local manifest the decisions were computed from; the executor
    /// passes it as the applier's `Overwrite::Expect` guard, proving the
    /// live tree still matches what was reconciled.
    pub guard_expected: Option<RootManifest>,
}

impl ActionPlan {
    /// True when executing can change nothing anywhere.
    pub fn is_empty(&self) -> bool {
        self.materialize.is_empty()
            && self.quarantine.is_empty()
            && self.send.is_empty()
            && self.fetch.is_empty()
            && self.conflicts.is_empty()
    }
}
