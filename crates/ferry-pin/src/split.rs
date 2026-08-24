//! The hold filter: what the exchange loop consults BEFORE applying a
//! reconcile plan ([`hold_filter`]), and the pure partition behind it
//! ([`split_plan`]).
//!
//! Given a plan computed against a peer's manifest, split it into:
//!
//! - **apply**: every decision on a path OUTSIDE the pin's globs. These
//!   run through the ordinary engine immediately — the pin is
//!   path-scoped, never folder-wide unless asked (`["*"]`).
//! - **held**: every decision on a PINNED path, recorded as [`HeldEntry`]
//!   ledger lines. Nothing about those paths touches the tree: A keeps
//!   living its versions until release.
//!
//! Two deliberate asymmetries (both documented in the ticket):
//!
//! - **fetch stays full.** The apply half keeps the ENTIRE fetch list, so
//!   the held versions' bytes land in the local store during the hold.
//!   Release can then materialize winners and quarantine losers without
//!   the peer being online — zero loss even if device B vanishes overnight.
//! - **send is scoped.** Chunks referenced exclusively by pinned paths are
//!   withheld from the send list: advertising A's pinned-path winners early
//!   would let the peer resolve the conflict unilaterally before release.
//!   Chunks shared with any unpinned path still travel (content-addressed
//!   chunks are inert without tree operations).
//!
//! Structural safety: if the two halves would move an ANCESTOR of one
//! another (e.g. the peer tore down a pinned directory itself), splitting
//! could break engine invariants — refuse loudly instead.

use std::collections::{BTreeMap, BTreeSet};

use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::manifest::{parse_tree_node, EntryPayload, RootManifest};
use ferry_store::store::Store;
use ferry_sync_engine::plan::{ActionPlan, LoserContent, Side};

use crate::error::PinError;
use crate::held::{HeldChunk, HeldEntry};
use crate::matcher::PathMatcher;
use crate::pin::PinStore;

/// What the pre-apply consultation decided.
#[derive(Debug)]
pub enum HoldDecision {
    /// No active pin (or a stale/released one): apply the plan as-is.
    Pass,
    /// An active pin held part of the plan.
    Hold(Box<SplitPlan>),
}

/// One round's partition.
#[derive(Debug)]
pub struct SplitPlan {
    /// The subset safe to execute now (guard and metadata preserved).
    pub apply: ActionPlan,
    /// Ledger entries for the pinned-path decisions, ready to append.
    pub held: Vec<HeldEntry>,
}

/// Seam used by the daemon/sync loop pre-apply. Loads the folder's pin,
/// ignores released/stale ones (surfaced elsewhere), and partitions the
/// plan when one actively holds. Corrupt pin state is a loud error —
/// never silently treated as "no pin".
pub fn hold_filter(
    state_dir: &std::path::Path,
    store: &Store,
    plan: &ActionPlan,
    local: &RootManifest,
    peer_device_hex: &str,
    remote_manifest_id_hex: &str,
    now: (i64, u32),
) -> Result<HoldDecision, PinError> {
    let Some(rec) = PinStore::new(state_dir).load()? else {
        return Ok(HoldDecision::Pass);
    };
    if !rec.holding() {
        return Ok(HoldDecision::Pass);
    }
    let matcher = PathMatcher::new(&rec.paths)?;
    let split = split_plan(
        plan,
        matcher,
        store,
        local,
        peer_device_hex,
        remote_manifest_id_hex,
        now,
    )?;
    if split.held.is_empty() {
        Ok(HoldDecision::Pass)
    } else {
        Ok(HoldDecision::Hold(Box::new(split)))
    }
}

/// Partition one plan against compiled pin globs.
#[allow(clippy::too_many_arguments)]
pub fn split_plan(
    plan: &ActionPlan,
    matcher: PathMatcher,
    store: &Store,
    local: &RootManifest,
    peer_device_hex: &str,
    remote_manifest_id_hex: &str,
    now: (i64, u32),
) -> Result<SplitPlan, PinError> {
    let held_mat: Vec<bool> = plan
        .materialize
        .iter()
        .map(|op| matcher.matches(&op.path))
        .collect();
    let held_qtn: Vec<bool> = plan
        .quarantine
        .iter()
        .map(|op| matcher.matches(&op.path))
        .collect();
    let held_con: Vec<bool> = plan
        .conflicts
        .iter()
        .map(|c| matcher.matches(&c.path))
        .collect();

    let any_held =
        held_mat.iter().any(|b| *b) || held_qtn.iter().any(|b| *b) || held_con.iter().any(|b| *b);
    if !any_held {
        return Ok(SplitPlan {
            apply: plan.clone(),
            held: Vec::new(),
        });
    }

    let mut held_keys: BTreeSet<String> = BTreeSet::new();
    for (op, h) in plan.materialize.iter().zip(&held_mat) {
        if *h {
            held_keys.insert(join(&op.path));
        }
    }
    for (op, h) in plan.quarantine.iter().zip(&held_qtn) {
        if *h {
            held_keys.insert(join(&op.path));
        }
    }
    for (c, h) in plan.conflicts.iter().zip(&held_con) {
        if *h {
            held_keys.insert(join(&c.path));
        }
    }

    let other_keys: Vec<String> = [
        plan.materialize
            .iter()
            .zip(&held_mat)
            .filter(|(_, h)| !**h)
            .map(|(op, _)| join(&op.path))
            .collect::<Vec<_>>(),
        plan.quarantine
            .iter()
            .zip(&held_qtn)
            .filter(|(_, h)| !**h)
            .map(|(op, _)| join(&op.path))
            .collect(),
        plan.conflicts
            .iter()
            .zip(&held_con)
            .filter(|(_, h)| !**h)
            .map(|(c, _)| join(&c.path))
            .collect(),
    ]
    .concat();

    // Refuse splits that would move one half inside the other: the engine's
    // structural suppression pass reasons over ONE plan, not two.
    for p in &held_keys {
        for q in &other_keys {
            if nests(p, q) || nests(q, p) {
                return Err(PinError::StructuralSplit {
                    pinned: p.clone(),
                    other: q.clone(),
                });
            }
        }
    }

    // Send-list scoping: withhold chunks referenced ONLY by pinned paths.
    let refs = chunk_path_map(store, local)?;
    let withheld = |id: &BlobId| -> bool {
        refs.get(id)
            .is_some_and(|paths| !paths.is_empty() && paths.iter().all(|p| held_keys.contains(p)))
    };
    let send = plan
        .send
        .iter()
        .filter(|(id, _)| !withheld(id))
        .cloned()
        .collect();

    let apply = ActionPlan {
        materialize: plan
            .materialize
            .iter()
            .zip(&held_mat)
            .filter(|(_, h)| !**h)
            .map(|(op, _)| op.clone())
            .collect(),
        quarantine: plan
            .quarantine
            .iter()
            .zip(&held_qtn)
            .filter(|(_, h)| !**h)
            .map(|(op, _)| op.clone())
            .collect(),
        send,
        // Full fetch: pull held versions' bytes NOW so release works offline.
        fetch: plan.fetch.clone(),
        conflicts: plan
            .conflicts
            .iter()
            .zip(&held_con)
            .filter(|(_, h)| !**h)
            .map(|(c, _)| c.clone())
            .collect(),
        guard_expected: plan.guard_expected.clone(),
    };

    // One ledger entry per distinct held path, merging what that path's
    // materialize / conflict / quarantine decisions say.
    let mut held = Vec::new();
    for key in &held_keys {
        let mat = plan
            .materialize
            .iter()
            .zip(&held_mat)
            .find(|(op, h)| **h && join(&op.path) == *key)
            .map(|(op, _)| op);
        let qtn = plan
            .quarantine
            .iter()
            .zip(&held_qtn)
            .find(|(op, h)| **h && join(&op.path) == *key)
            .map(|(op, _)| op);
        let con = plan
            .conflicts
            .iter()
            .zip(&held_con)
            .find(|(c, h)| **h && join(&c.path) == *key)
            .map(|(c, _)| c);

        let (decision, conflict_winner) = match con {
            Some(c) => ("conflict".to_string(), Some(side_str(c.winner))),
            None => match mat.map(|m| &m.result) {
                Some(Some(_)) => ("remote_apply".to_string(), None),
                Some(None) => ("remote_delete".to_string(), None),
                // Quarantine-only (cannot actually happen: every planned
                // conflict carries a winner decision); classify safely.
                None => ("conflict".to_string(), None),
            },
        };

        // The held version's blob refs: the remote state we refused to
        // apply (materialize result), or the remote loser copy rebuilt from
        // the store (FromStore quarantine content). Empty means deletion.
        let mut chunks: Vec<HeldChunk> = Vec::new();
        match mat.and_then(|m| m.result.as_ref()) {
            Some(state) => chunks.extend(state.chunks.iter().map(|(id, len)| HeldChunk {
                id: hex(id),
                len: *len,
            })),
            None => {
                if let Some(LoserContent::FromStore { chunks: cs, .. }) = qtn.map(|q| &q.content) {
                    chunks.extend(cs.iter().map(|(id, len)| HeldChunk {
                        id: hex(id),
                        len: *len,
                    }));
                }
            }
        }

        held.push(HeldEntry {
            held_sec: now.0,
            held_nsec: now.1,
            path: key.clone(),
            device_id: peer_device_hex.to_string(),
            remote_manifest_id: remote_manifest_id_hex.to_string(),
            chunks,
            decision,
            conflict_winner: conflict_winner.map(str::to_string),
        });
    }

    Ok(SplitPlan { apply, held })
}

/// True when `prefix` is a strict ancestor path of `whole`.
fn nests(prefix: &str, whole: &str) -> bool {
    whole.len() > prefix.len()
        && whole.starts_with(prefix)
        && whole[prefix.len()..].starts_with('/')
}

fn join(parts: &[String]) -> String {
    parts.join("/")
}

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Local => "local",
        Side::Remote => "remote",
    }
}

/// Map every data chunk in a manifest to the set of stored paths that
/// reference it (used to attribute send candidates to pinned paths).
fn chunk_path_map(
    store: &Store,
    manifest: &RootManifest,
) -> Result<BTreeMap<BlobId, BTreeSet<String>>, PinError> {
    let mut out: BTreeMap<BlobId, BTreeSet<String>> = BTreeMap::new();
    // DFS over tree nodes carrying the stored-path prefix each lives at.
    let mut work: Vec<(BlobId, Vec<String>)> = vec![(manifest.root_tree_id, Vec::new())];
    while let Some((tree_id, prefix)) = work.pop() {
        let bytes = store
            .get(BlobKind::TreeNode, &tree_id)
            .map_err(PinError::Store)?;
        let node = parse_tree_node(&bytes).map_err(PinError::Manifest)?;
        for e in node.entries {
            let mut path = prefix.clone();
            path.push(e.name);
            match e.payload {
                EntryPayload::File { chunks, .. } => {
                    let joined = join(&path);
                    for (id, _) in chunks {
                        out.entry(id).or_default().insert(joined.clone());
                    }
                }
                EntryPayload::Dir { child_tree_id } => work.push((child_tree_id, path)),
                EntryPayload::Symlink { .. } => {}
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_store::diff::EntryState;
    use ferry_sync_engine::plan::{ConflictKind, MaterializeOp, PlannedConflict};
    use rand::SeedableRng;

    fn st(chunks: Vec<(BlobId, u64)>) -> EntryState {
        EntryState {
            kind: ferry_store::diff::EntryKind::File,
            exec: false,
            mtime_sec: 1,
            mtime_nsec: 0,
            chunks,
            target: None,
        }
    }

    fn plan_with(paths: &[&str]) -> ActionPlan {
        let mut plan = ActionPlan::default();
        for p in paths {
            plan.materialize.push(MaterializeOp {
                path: p.split('/').map(str::to_string).collect(),
                base: None,
                result: Some(st(Vec::new())),
            });
        }
        plan
    }

    fn matcher(patterns: &[&str]) -> PathMatcher {
        PathMatcher::new(&patterns.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn disjoint_paths_pass_through_untouched() {
        let (store, manifest) = store_with_empty_tree();
        let plan = plan_with(&["docs/readme.md", "notes.txt"]);
        let split = split_plan(
            &plan,
            matcher(&["src/**"]),
            &store,
            &manifest,
            "bb",
            "cc",
            (1, 0),
        )
        .unwrap();
        assert!(split.held.is_empty());
        assert_eq!(split.apply.materialize.len(), 2);
    }

    #[test]
    fn nested_halves_are_refused_loudly() {
        let (store, manifest) = store_with_empty_tree();
        // Peer tears down `src` itself while `src/inner.rs` is pinned.
        let mut plan = plan_with(&["src/inner.rs"]);
        plan.materialize.insert(
            0,
            MaterializeOp {
                path: vec!["src".into()],
                base: None,
                result: None, // deletion of the ancestor
            },
        );
        let err = split_plan(
            &plan,
            matcher(&["src/inner.rs"]),
            &store,
            &manifest,
            "bb",
            "cc",
            (1, 0),
        )
        .unwrap_err();
        assert!(matches!(err, PinError::StructuralSplit { .. }), "{err}");
    }

    #[test]
    fn fetch_list_survives_the_split_in_full_and_paths_partition() {
        let (store, manifest) = store_with_empty_tree();
        let mut plan = plan_with(&["docs/readme.md", "src/main.rs"]);
        plan.fetch.push(([9u8; 32], 12));
        plan.fetch.push(([8u8; 32], 34));
        let split = split_plan(
            &plan,
            matcher(&["src/**"]),
            &store,
            &manifest,
            "bb",
            "cc",
            (1, 0),
        )
        .unwrap();
        assert_eq!(split.apply.fetch.len(), 2, "held bytes ride along");
        assert_eq!(split.held.len(), 1);
        assert_eq!(split.held[0].decision, "remote_apply");
        assert_eq!(
            join(&split.apply.materialize[0].path),
            "docs/readme.md",
            "unpinned path still applies"
        );
    }

    #[test]
    fn star_pin_holds_everything() {
        let (store, manifest) = store_with_empty_tree();
        let plan = plan_with(&["a.txt", "deep/b.txt"]);
        let split = split_plan(
            &plan,
            matcher(&["*"]),
            &store,
            &manifest,
            "bb",
            "cc",
            (1, 0),
        )
        .unwrap();
        assert_eq!(split.held.len(), 2);
        assert!(split.apply.materialize.is_empty());
    }

    #[test]
    fn conflict_decisions_carry_winner_into_the_ledger_line() {
        let (store, manifest) = store_with_empty_tree();
        let mut plan = plan_with(&[]);
        plan.conflicts.push(PlannedConflict {
            path: vec!["src".to_string(), "main.rs".to_string()],
            kind: ConflictKind::BothChanged,
            winner: Side::Local,
            loser: Side::Remote,
            winner_device: [1; 32],
            loser_device: [2; 32],
            winner_mtime_sec: 10,
            winner_mtime_nsec: 0,
            loser_mtime_sec: Some(5),
            loser_mtime_nsec: Some(0),
            quarantined_as: None,
        });
        let split = split_plan(
            &plan,
            matcher(&["src/**"]),
            &store,
            &manifest,
            "bb",
            "cc",
            (1, 0),
        )
        .unwrap();
        assert_eq!(split.held[0].decision, "conflict");
        assert_eq!(split.held[0].conflict_winner.as_deref(), Some("local"));
    }

    /// Minimal stand-ins; send-attribution and full round-trips live in the
    /// scenario integration where real stores build real manifests. The
    /// store/manifest pair IS real (an empty tree) because split_plan's
    /// send-attribution walks the local tree.
    fn store_with_empty_tree() -> (Store, RootManifest) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.keep();
        let store_root = root.join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = Store::create(
            &store_root,
            [7u8; 32],
            Box::new(ferry_store::crypto::PassthroughCipher),
        )
        .unwrap();
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        let idn = ferry_store::snapshot::SnapshotIdentity {
            folder_id: [7; 16],
            device_id: [9; 32],
            parent_manifest_id: [0; 32],
            created_sec: 1_787_000_000,
            created_nsec: 0,
        };
        let out = ferry_store::snapshot::snapshot_dir(
            &store,
            ferry_store::chunker::generate_polynomial(&mut rand::rngs::StdRng::seed_from_u64(3)),
            &tree,
            &idn,
        )
        .unwrap();
        (store, out.manifest)
    }
}
