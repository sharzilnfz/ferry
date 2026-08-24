//! The v1 engine's materializer seam: one thin adapter over
//! [`ferry_materialize::Applier`].
//!
//! There is exactly one applier in this workspace. The v1 pull stages
//! (both [`crate::engine`] and [`crate::exchange`]) route every
//! materialization through here, which means the engine path inherits —
//! and is regression-tested against — Applier's full contract: atomic
//! temp+rename writes, chunk hash verification, children-first deletions,
//! parents-first creations, exact file/symlink/DIR mtimes, NFC live-name
//! folding, component validation, and the untrusted-symlink-target policy
//! ([`ferry_platform::classify_link`] plus the windows dir-link gate) that
//! refuses absolute, escaping, and drive-prefixed targets loudly.
//!
//! The dir-mtime restoration from the target tree (the old phase-3
//! contract) rides inside
//! [`Applier::apply_session_change_set`][ferry_materialize::Applier::apply_session_change_set].

use std::path::PathBuf;

use ferry_materialize::MaterializeError;
use ferry_store::diff::ChangeSet;
use ferry_store::manifest::RootManifest;
use ferry_store::store::Store;

/// Applies fetched change sets to one working tree on behalf of a sync
/// session, reading blobs from the local store.
pub struct SessionApplier<'a> {
    store: &'a Store,
    root: PathBuf,
}

impl<'a> SessionApplier<'a> {
    /// New session applier writing under `root`, reading blobs from
    /// `store`.
    pub fn new(store: &'a Store, root: impl Into<PathBuf>) -> Self {
        SessionApplier {
            store,
            root: root.into(),
        }
    }

    /// Bring the working tree to `target`'s state by applying exactly
    /// `changes` (every blob they reference must already be in `store`),
    /// then restore every directory mtime from the target tree so the next
    /// local snapshot reproduces the agreed root id byte-for-byte.
    pub fn apply(
        &mut self,
        target: &RootManifest,
        changes: &ChangeSet,
    ) -> Result<(), MaterializeError> {
        let mut applier = ferry_materialize::Applier::new(self.store, &self.root);
        applier.apply_session_change_set(changes, &target.root_tree_id)?;
        Ok(())
    }
}
