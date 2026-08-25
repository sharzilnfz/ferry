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
//! This is also THE execution boundary for session pinning (T-06): the one
//! place sessions mutate a working tree, running after fetch completes and
//! immediately before anything materializes. With
//! [`SessionApplier::pin_enforcement`] configured, the folder's pin record
//! is re-read HERE — closing the fetch-to-apply TOCTOU — and every changed
//! path inside the pin's globs is withheld from the tree and ledgered under
//! `.ferry/held/<peer>.jsonl`, exactly what release reconciles later. Every
//! driver that applies peer content inherits enforcement; without it the
//! policy defaults to no-pin and nothing changes.
//!
//! The dir-mtime restoration from the target tree (the old phase-3
//! contract) rides inside
//! [`Applier::apply_session_change_set`][ferry_materialize::Applier::apply_session_change_set].

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ferry_materialize::MaterializeError;
use ferry_pin::{HeldChunk, HeldEntry, HeldLedger, PathMatcher, PinError, PinStore};
use ferry_store::diff::{join_path, ChangeSet};
use ferry_store::manifest::RootManifest;
use ferry_store::store::Store;

/// Applies fetched change sets to one working tree on behalf of a sync
/// session, reading blobs from the local store.
pub struct SessionApplier<'a> {
    store: &'a Store,
    root: PathBuf,
    pin: Option<PinCtx>,
}

/// Everything the boundary needs to consult and feed the pin subsystem.
#[derive(Clone, Debug)]
struct PinCtx {
    /// The folder's `.ferry` directory (pin state + held ledgers).
    state_dir: PathBuf,
    /// 64-hex device id of the peer whose changes are being applied.
    peer_hex: String,
    /// 64-hex id of the remote manifest this change set came from.
    remote_manifest_id_hex: String,
}

/// Failure modes of [`SessionApplier::apply`]: the materializer's own
/// contract violations plus loud pin-state errors (a corrupt pin file never
/// silently means "no pin").
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Materialize(#[from] MaterializeError),
    #[error("pin: {0}")]
    Pin(#[from] PinError),
}

/// What one apply actually did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Distinct paths an active pin withheld from the tree this round.
    /// Zero proves nothing was held. Callers treat any nonzero as "the peer
    /// state was only partially accepted".
    pub held: usize,
}

impl<'a> SessionApplier<'a> {
    /// New session applier writing under `root`, reading blobs from
    /// `store`. Default policy: no-pin — materialization never consults
    /// pin state until [`SessionApplier::pin_enforcement`] says otherwise.
    pub fn new(store: &'a Store, root: impl Into<PathBuf>) -> Self {
        SessionApplier {
            store,
            root: root.into(),
            pin: None,
        }
    }

    /// Enforce session pinning at this boundary (T-06): before mutating the
    /// tree, re-read `<state_dir>/pin-state.json`; while it actively holds,
    /// withhold every changed path matching its globs from materialization
    /// and ledger them for `peer_hex` under `remote_manifest_id_hex`.
    pub fn pin_enforcement(
        mut self,
        state_dir: impl Into<PathBuf>,
        peer_hex: impl Into<String>,
        remote_manifest_id_hex: impl Into<String>,
    ) -> Self {
        self.pin = Some(PinCtx {
            state_dir: state_dir.into(),
            peer_hex: peer_hex.into(),
            remote_manifest_id_hex: remote_manifest_id_hex.into(),
        });
        self
    }

    /// Bring the working tree to `target`'s state by applying exactly
    /// `changes` (every blob they reference must already be in `store`),
    /// then restore every directory mtime from the target tree so the next
    /// local snapshot reproduces the agreed root id byte-for-byte.
    ///
    /// Under pin enforcement the applied set is `changes` MINUS whatever
    /// the active pin withholds; see [`ApplyOutcome::held`].
    pub fn apply(
        &mut self,
        target: &RootManifest,
        changes: &ChangeSet,
    ) -> Result<ApplyOutcome, ApplyError> {
        let (to_apply, outcome) = match &self.pin {
            Some(ctx) => enforce_pin(ctx, changes)?,
            None => (changes.clone(), ApplyOutcome::default()),
        };
        let mut applier = ferry_materialize::Applier::new(self.store, &self.root);
        applier.apply_session_change_set(&to_apply, &target.root_tree_id)?;
        Ok(outcome)
    }
}

/// The boundary consultation itself: fresh pin read → holding check → glob
/// partition → ledger append. Returns the change set that may touch the
/// tree plus how many paths were withheld.
fn enforce_pin(ctx: &PinCtx, changes: &ChangeSet) -> Result<(ChangeSet, ApplyOutcome), PinError> {
    // Tolerant fast path: no pin on disk at all is the steady state of
    // most folders; one stat avoids the parse entirely.
    if !PinStore::new(&ctx.state_dir).path().is_file() {
        return Ok((changes.clone(), ApplyOutcome::default()));
    }
    let Some(rec) = PinStore::new(&ctx.state_dir).load()? else {
        return Ok((changes.clone(), ApplyOutcome::default()));
    };
    if !rec.holding() {
        return Ok((changes.clone(), ApplyOutcome::default()));
    }
    let matcher = PathMatcher::new(&rec.paths)?;

    // Partition each diff bucket by the pin's globs. Held paths are merged
    // per stored path (one decision per path, mirroring ferry-pin's
    // split_plan); deletions win only when nothing else says otherwise,
    // which cannot collide anyway — the differ reports a path in exactly
    // one bucket. No structural refusal here: change-set ops are already
    // flattened PER PATH (parents before children), so withholding some
    // leaves never moves an ancestor of an applied sibling.
    enum Held {
        RemoteApply(Vec<HeldChunk>),
        RemoteDelete,
    }
    let mut apply_cs = ChangeSet::default();
    let mut held: BTreeMap<String, Held> = BTreeMap::new();

    for a in &changes.added {
        if matcher.matches(&a.path) {
            held.insert(
                join_path(&a.path),
                Held::RemoteApply(
                    a.state
                        .chunks
                        .iter()
                        .map(|(id, len)| chunk(id, *len))
                        .collect(),
                ),
            );
        } else {
            apply_cs.added.push(a.clone());
        }
    }
    for r in &changes.removed {
        if matcher.matches(&r.path) {
            held.insert(join_path(&r.path), Held::RemoteDelete);
        } else {
            apply_cs.removed.push(r.clone());
        }
    }
    // Each modified bucket stays in its own bucket on the apply side so
    // metadata/type classifications survive the split untouched.
    for m in &changes.content_modified {
        if matcher.matches(&m.path) {
            held.insert(
                join_path(&m.path),
                Held::RemoteApply(
                    m.after
                        .chunks
                        .iter()
                        .map(|(id, len)| chunk(id, *len))
                        .collect(),
                ),
            );
        } else {
            apply_cs.content_modified.push(m.clone());
        }
    }
    for m in &changes.type_changed {
        if matcher.matches(&m.path) {
            held.insert(
                join_path(&m.path),
                Held::RemoteApply(
                    m.after
                        .chunks
                        .iter()
                        .map(|(id, len)| chunk(id, *len))
                        .collect(),
                ),
            );
        } else {
            apply_cs.type_changed.push(m.clone());
        }
    }
    for m in &changes.metadata_modified {
        if matcher.matches(&m.path) {
            held.insert(
                join_path(&m.path),
                Held::RemoteApply(
                    m.after
                        .chunks
                        .iter()
                        .map(|(id, len)| chunk(id, *len))
                        .collect(),
                ),
            );
        } else {
            apply_cs.metadata_modified.push(m.clone());
        }
    }

    if held.is_empty() {
        return Ok((apply_cs, ApplyOutcome { held: 0 }));
    }

    // Surface the holds: one ledger line per distinct path, deduped against
    // what earlier rounds already recorded for THIS manifest so a long pin
    // across many poll ticks appends nothing new. The fetch ran in full
    // upstream, so the held versions' bytes are already in the store and
    // release works offline.
    let now = now_parts();
    let entries: Vec<HeldEntry> = held
        .into_iter()
        .map(|(path, kind)| {
            let (decision, chunks) = match kind {
                Held::RemoteApply(chunks) => ("remote_apply".to_string(), chunks),
                Held::RemoteDelete => ("remote_delete".to_string(), Vec::new()), // empty = deletion
            };
            HeldEntry {
                held_sec: now.0,
                held_nsec: now.1,
                path,
                device_id: ctx.peer_hex.clone(),
                remote_manifest_id: ctx.remote_manifest_id_hex.clone(),
                chunks,
                decision,
                conflict_winner: None, // conflicts are planned above this layer
            }
        })
        .collect();
    let held_count = entries.len();
    let ledger = HeldLedger::new(&ctx.state_dir);
    let known: BTreeSet<(String, String)> = ledger
        .load_peer(&ctx.peer_hex)?
        .into_iter()
        .map(|e| (e.path, e.remote_manifest_id))
        .collect();
    let fresh: Vec<HeldEntry> = entries
        .into_iter()
        .filter(|e| !known.contains(&(e.path.clone(), e.remote_manifest_id.clone())))
        .collect();
    ledger.append(&ctx.peer_hex, &fresh)?;

    Ok((apply_cs, ApplyOutcome { held: held_count }))
}

fn chunk(id: &ferry_store::format::BlobId, len: u64) -> HeldChunk {
    HeldChunk {
        id: ferry_store::format::hex(id),
        len,
    }
}

/// Wall-clock `(sec, nsec)` for ledger stamps. Local copy: ferry-sync does
/// not depend on ferry-sync-engine, whose `timefmt::now_unix` the CLI uses
/// for the same purpose.
fn now_parts() -> (i64, u32) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::diff::{Added, CompPath, EntryKind, EntryState, Modified, Removed};
    use ferry_store::format::BlobId;

    fn state(chunks: Vec<(BlobId, u64)>) -> EntryState {
        EntryState {
            kind: EntryKind::File,
            exec: false,
            mtime_sec: 1,
            mtime_nsec: 0,
            chunks,
            target: None,
        }
    }

    fn comp(path: &str) -> CompPath {
        path.split('/').map(str::to_string).collect()
    }

    fn sample_changes() -> ChangeSet {
        let mut cs = ChangeSet::default();
        cs.added.push(Added {
            path: comp("notes.txt"),
            state: state(vec![([1u8; 32], 4)]),
        });
        cs.removed.push(Removed {
            path: comp("docs/old.txt"),
            state: state(Vec::new()),
        });
        cs.content_modified.push(Modified {
            path: comp("docs/readme.md"),
            before: state(Vec::new()),
            after: state(vec![([2u8; 32], 9)]),
        });
        cs.metadata_modified.push(Modified {
            path: comp("src/lib.rs"),
            before: state(vec![([3u8; 32], 1)]),
            after: state(vec![([3u8; 32], 1)]),
        });
        cs
    }

    fn pin_ctx(state_dir: &std::path::Path) -> PinCtx {
        PinCtx {
            state_dir: state_dir.to_path_buf(),
            peer_hex: "bb".repeat(32),
            remote_manifest_id_hex: "cc".repeat(32),
        }
    }

    #[test]
    fn without_a_pin_everything_passes_through() {
        let dir = tempfile::tempdir().unwrap();
        let changes = sample_changes();
        let (out, outcome) = enforce_pin(&pin_ctx(dir.path()), &changes).unwrap();
        assert_eq!(outcome.held, 0);
        assert_eq!(out, changes, "no-pin policy applies the full set");
    }

    #[test]
    fn released_or_stale_pins_hold_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path());
        let mut rec = ferry_pin::PinRecord {
            format_version: ferry_pin::PIN_FORMAT_VERSION,
            device_id: "aa".repeat(32),
            pid: std::process::id(),
            started_sec: 1,
            started_nsec: 0,
            paths: vec!["*".into()],
            released: true, // ended: passes through even though scoped "*"
            base_agreements: BTreeMap::new(),
            proc_start_token: None,
        };
        store.start(&rec).unwrap();
        let (out, outcome) = enforce_pin(&pin_ctx(dir.path()), &sample_changes()).unwrap();
        assert_eq!(outcome.held, 0);
        assert_eq!(out.added.len(), 1);

        rec.released = false;
        rec.pid = 4_000_000_000; // dead writer: stale pins expire too
        store.start(&rec).unwrap();
        let (_, outcome) = enforce_pin(&pin_ctx(dir.path()), &sample_changes()).unwrap();
        assert_eq!(outcome.held, 0);
    }

    #[test]
    fn pinned_paths_are_withheld_ledgered_and_unpinned_still_apply() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path());
        store
            .start(&ferry_pin::PinRecord {
                format_version: ferry_pin::PIN_FORMAT_VERSION,
                device_id: "aa".repeat(32),
                pid: std::process::id(), // alive + auto-stamped => holding
                started_sec: 1,
                started_nsec: 0,
                paths: vec!["notes.txt".to_string(), "docs/**".to_string()],
                released: false,
                base_agreements: BTreeMap::new(),
                proc_start_token: None,
            })
            .unwrap();

        let (out, outcome) = enforce_pin(&pin_ctx(dir.path()), &sample_changes()).unwrap();
        assert_eq!(outcome.held, 3, "added + removed + content_modified held");
        assert!(out.added.is_empty() && out.removed.is_empty());
        assert_eq!(
            out.content_modified.len() + out.metadata_modified.len(),
            1,
            "only the unpinned metadata edit survives"
        );
        assert_eq!(join_path(&out.metadata_modified[0].path), "src/lib.rs");

        // Surfaced: readable by the same machinery release consumes.
        let ledger = HeldLedger::new(dir.path());
        let entries = ledger.load_peer(&"bb".repeat(32)).unwrap();
        assert_eq!(entries.len(), 3);
        let notes = entries.iter().find(|e| e.path == "notes.txt").unwrap();
        assert_eq!(notes.decision, "remote_apply");
        assert_eq!(notes.chunks.len(), 1);
        assert_eq!(notes.chunks[0].len, 4);
        assert_eq!(notes.remote_manifest_id, "cc".repeat(32));
        let old = entries.iter().find(|e| e.path == "docs/old.txt").unwrap();
        assert_eq!(old.decision, "remote_delete");
        assert!(old.chunks.is_empty(), "empty chunks mean deletion");

        // A second identical round appends NOTHING: long pins across many
        // poll ticks must not grow the ledger.
        let (_, again) = enforce_pin(&pin_ctx(dir.path()), &sample_changes()).unwrap();
        assert_eq!(again.held, 3, "still withheld");
        assert_eq!(ledger.load_peer(&"bb".repeat(32)).unwrap().len(), 3);
    }
}
