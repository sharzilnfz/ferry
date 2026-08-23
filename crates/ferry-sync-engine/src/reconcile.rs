//! The three-way reconciliation engine (ADR-0004).
//!
//! Inputs are three manifests — local current, remote current, last-agreed
//! base (None = initial sync, empty-tree base) — all readable through one
//! store. Output is an [`ActionPlan`](crate::plan::ActionPlan). This module
//! never touches the live filesystem; mutation happens in [`crate::execute`].
//!
//! Per-path decision table (states are `Option<EntryState>`; "changed"
//! means differs from base):
//!
//! | base→local | base→remote | decision |
//! |---|---|---|
//! | unchanged | unchanged | nothing |
//! | changed | unchanged | local wins; winner chunks go on the send list |
//! | unchanged | changed | apply remote locally |
//! | changed | changed identically | nothing (already converged) |
//! | metadata-only change | metadata-only change | deterministic winner (newer mtime, device tiebreak); silent, no conflict file |
//! | deleted | edited | edit resurrects live; report entry |
//! | edited | deleted | mirror of the above |
//! | deleted | deleted | stays deleted |
//! | changed differently | changed differently | CONFLICT: newer mtime wins, exact ties go to the higher manifest device id; loser quarantined |
//!
//! Directory paths never rule on themselves: both sides being dirs is
//! settled by their individually-enumerated children, so disjoint additions
//! union cleanly.
//!
//! Deletions never destroy locally-winning content: when a path below a
//! remotely-deleted directory wins its own conflict (or is quarantined), the
//! ancestor removal is suppressed and the manifest exchange carries the
//! resurrection instead.

use std::collections::{BTreeMap, BTreeSet};

use ferry_store::diff::{diff_roots, CompPath, EntryKind, EntryState};
use ferry_store::format::{BlobId, BlobKind};
use ferry_store::manifest::{
    parse_tree_node, serialize_tree_node, EntryPayload, ManifestError, RootManifest, TreeNode,
};
use ferry_store::store::{Store, StoreError};
use thiserror::Error;

use crate::plan::{
    ActionPlan, ConflictKind, LoserContent, MaterializeOp, PlannedConflict, QuarantineOp, Side,
};

#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("manifest decode failed: {0}")]
    Manifest(#[from] ManifestError),
    #[error("diff failed: {0}")]
    Diff(#[from] ferry_store::diff::DiffError),
    #[error(
        "structural conflict: {ancestor} is replaced or torn down by one side while {path} \
         changes beneath it; merging across levels is unsupported in v1 and refusing beats \
         guessing"
    )]
    StructuralConflict { ancestor: String, path: String },
}

/// One reconcile computation.
pub struct ReconcileInput<'a> {
    /// Store holding both sides' metadata blobs (the transport puts received
    /// manifests and tree nodes here) plus data chunks.
    pub store: &'a Store,
    pub local: &'a RootManifest,
    pub remote: &'a RootManifest,
    /// Last-agreed ancestor; `None` means initial sync against an empty tree.
    pub base: Option<&'a RootManifest>,
}

/// What one side's diff says about one path.
#[derive(Clone, Debug, Default)]
struct SideView {
    base: Option<EntryState>,
    side: Option<EntryState>,
}

type ViewMap = BTreeMap<String, (CompPath, SideView)>;

fn index_change_set(cs: &ferry_store::diff::ChangeSet) -> ViewMap {
    let mut map: ViewMap = BTreeMap::new();
    let mut slot = |path: &CompPath, base: Option<EntryState>, side: Option<EntryState>| {
        map.insert(path.join("/"), (path.clone(), SideView { base, side }));
    };
    for a in &cs.added {
        slot(&a.path, None, Some(a.state.clone()));
    }
    for r in &cs.removed {
        slot(&r.path, Some(r.state.clone()), None);
    }
    for m in cs
        .content_modified
        .iter()
        .chain(&cs.metadata_modified)
        .chain(&cs.type_changed)
    {
        slot(&m.path, Some(m.before.clone()), Some(m.after.clone()));
    }
    map
}

/// True when both sides carry the same bytes (chunk sequence for files,
/// target for symlinks); kind must match.
fn same_content(l: &EntryState, r: &EntryState) -> bool {
    l.kind == r.kind
        && match l.kind {
            EntryKind::Symlink => l.target == r.target,
            _ => l.chunks == r.chunks,
        }
}

/// Newer mtime wins; exact ties go to the higher manifest device id. The
/// comparison is symmetric, so both devices pick the same winner.
fn pick_winner(l: &EntryState, r: &EntryState, local_dev: &[u8; 32], remote_dev: &[u8; 32]) -> Side {
    let l_mt = (l.mtime_sec, l.mtime_nsec);
    let r_mt = (r.mtime_sec, r.mtime_nsec);
    if l_mt != r_mt {
        return if l_mt > r_mt { Side::Local } else { Side::Remote };
    }
    if local_dev > remote_dev {
        Side::Local
    } else {
        Side::Remote
    }
}

enum Decision {
    Nothing,
    /// Silent remote win (including metadata-only resolution).
    ApplyRemote,
    /// Silent local win (nothing to do locally; content goes on the send list).
    KeepLocal,
    /// Real divergence; `winner` keeps its bytes live at the path.
    Conflict { kind: ConflictKind, winner: Side },
}

/// Every chunk id referenced by any file in a manifest's trees.
fn manifest_chunk_refs(store: &Store, root: &BlobId) -> Result<BTreeSet<BlobId>, ReconcileError> {
    let mut seen_trees: BTreeSet<BlobId> = BTreeSet::new();
    let mut out = BTreeSet::new();
    let mut stack = vec![*root];
    while let Some(id) = stack.pop() {
        if !seen_trees.insert(id) {
            continue;
        }
        let node = parse_tree_node(&store.get(BlobKind::TreeNode, &id)?)?;
        for e in node.entries {
            match e.payload {
                EntryPayload::File { chunks, .. } => out.extend(chunks.into_iter().map(|c| c.0)),
                EntryPayload::Dir { child_tree_id } => stack.push(child_tree_id),
                EntryPayload::Symlink { .. } => {}
            }
        }
    }
    Ok(out)
}

fn join(parts: &[String]) -> String {
    parts.join("/")
}

fn collect_chunks(state: &EntryState, out: &mut BTreeMap<BlobId, u64>) {
    for (id, len) in &state.chunks {
        out.insert(*id, *len);
    }
}

/// Run one reconciliation and produce the executable plan.
pub fn reconcile(input: ReconcileInput<'_>) -> Result<ActionPlan, ReconcileError> {
    let ReconcileInput {
        store,
        local,
        remote,
        base,
    } = input;

    // Base root: the agreed manifest's tree, or the empty tree for initial
    // sync (add-vs-add treats the empty tree as the ancestor).
    let empty_root = store.put_meta(BlobKind::TreeNode, &serialize_tree_node(&TreeNode::default()))?;
    let base_root = base.map(|m| m.root_tree_id).unwrap_or(empty_root);

    let local_view = index_change_set(&diff_roots(store, &base_root, &local.root_tree_id)?);
    let remote_view = index_change_set(&diff_roots(store, &base_root, &remote.root_tree_id)?);

    let mut keys: BTreeSet<String> = local_view.keys().cloned().collect();
    keys.extend(remote_view.keys().cloned());

    let mut decisions: BTreeMap<String, Decision> = BTreeMap::new();
    let mut materialize: Vec<MaterializeOp> = Vec::new();
    let mut quarantine: Vec<QuarantineOp> = Vec::new();
    let mut conflicts: Vec<PlannedConflict> = Vec::new();
    let mut send_cand: BTreeMap<BlobId, u64> = BTreeMap::new();
    let mut fetch_cand: BTreeMap<BlobId, u64> = BTreeMap::new();

    for key in &keys {
        let lv = local_view.get(key);
        let rv = remote_view.get(key);
        let path = lv
            .or(rv)
            .expect("every key comes from at least one view")
            .0
            .clone();
        let b = lv
            .and_then(|(_, v)| v.base.clone())
            .or_else(|| rv.and_then(|(_, v)| v.base.clone()));
        let l = lv.and_then(|(_, v)| v.side.clone());
        let r = rv.and_then(|(_, v)| v.side.clone());

        let local_changed = l != b;
        let remote_changed = r != b;

        let decision = match (local_changed, remote_changed) {
            (false, false) => Decision::Nothing,
            (true, false) => Decision::KeepLocal,
            (false, true) => Decision::ApplyRemote,
            (true, true) => {
                if l == r {
                    // Both changed identically: converge silently.
                    Decision::Nothing
                } else {
                    match (&l, &r) {
                        (Some(ls), Some(rs)) => {
                            if same_content(ls, rs) {
                                // Metadata-only divergence (mtime/exec):
                                // deterministic and silent, no bytes at risk.
                                if pick_winner(ls, rs, &local.device_id, &remote.device_id)
                                    == Side::Local
                                {
                                    Decision::KeepLocal
                                } else {
                                    Decision::ApplyRemote
                                }
                            } else if ls.kind == EntryKind::Dir && rs.kind == EntryKind::Dir {
                                // Two directories: children decide themselves.
                                Decision::Nothing
                            } else {
                                let kind = if l.is_none() || r.is_none() {
                                    // Exactly one side is a deletion; the
                                    // other necessarily differs from base.
                                    ConflictKind::DeleteVsEdit
                                } else if b.is_none() {
                                    ConflictKind::AddVsAdd
                                } else {
                                    ConflictKind::BothChanged
                                };
                                let winner = match kind {
                                    // The edit always beats the deletion.
                                    ConflictKind::DeleteVsEdit => {
                                        if l.is_some() { Side::Local } else { Side::Remote }
                                    }
                                    _ => pick_winner(
                                        ls,
                                        rs,
                                        &local.device_id,
                                        &remote.device_id,
                                    ),
                                };
                                Decision::Conflict { kind, winner }
                            }
                        }
                        _ => {
                            // One side deleted, the other edited (both-deleted
                            // was caught by l == r).
                            Decision::Conflict {
                                kind: ConflictKind::DeleteVsEdit,
                                winner: if l.is_some() { Side::Local } else { Side::Remote },
                            }
                        }
                    }
                }
            }
        };

        match &decision {
            Decision::Nothing => {}
            Decision::KeepLocal => {
                if let Some(ls) = &l {
                    collect_chunks(ls, &mut send_cand);
                }
            }
            Decision::ApplyRemote => {
                if let Some(rs) = &r {
                    collect_chunks(rs, &mut fetch_cand);
                }
                materialize.push(MaterializeOp {
                    path: path.clone(),
                    base: b.clone(),
                    result: r.clone(),
                });
            }
            Decision::Conflict { kind, winner } => {
                let loser_side = match winner {
                    Side::Local => Side::Remote,
                    Side::Remote => Side::Local,
                };
                let (w_state, lost) = match winner {
                    Side::Local => (l.as_ref(), r.as_ref()),
                    Side::Remote => (r.as_ref(), l.as_ref()),
                };
                conflicts.push(PlannedConflict {
                    path: path.clone(),
                    kind: *kind,
                    winner: *winner,
                    loser: loser_side,
                    winner_device: match winner {
                        Side::Local => local.device_id,
                        Side::Remote => remote.device_id,
                    },
                    loser_device: match loser_side {
                        Side::Local => local.device_id,
                        Side::Remote => remote.device_id,
                    },
                    winner_mtime_sec: w_state.map(|s| s.mtime_sec).unwrap_or(0),
                    winner_mtime_nsec: w_state.map(|s| s.mtime_nsec).unwrap_or(0),
                    loser_mtime_sec: lost.as_ref().map(|s| s.mtime_sec),
                    loser_mtime_nsec: lost.as_ref().map(|s| s.mtime_nsec),
                    quarantined_as: None,
                });
                match winner {
                    Side::Local => {
                        if let Some(w) = w_state {
                            collect_chunks(w, &mut send_cand);
                        }
                    }
                    Side::Remote => {
                        if let Some(w) = w_state {
                            collect_chunks(w, &mut fetch_cand);
                        }
                        materialize.push(MaterializeOp {
                            path: path.clone(),
                            base: b.clone(),
                            result: r.clone(),
                        });
                    }
                }
                // Quarantine the loser's bytes when the loser HAS bytes.
                if let Some(loser_state) = lost {
                    if loser_state.kind == EntryKind::Dir {
                        // Refusing to quarantine a whole subtree; nothing
                        // will be mutated because the plan aborts here.
                        return Err(ReconcileError::StructuralConflict {
                            ancestor: join(&path),
                            path: join(&path),
                        });
                    }
                    let (loser_dev, content) = match loser_side {
                        Side::Local => {
                            let content = if loser_state.kind == EntryKind::Symlink {
                                LoserContent::LiveLocalSymlink {
                                    expected_target: loser_state.target.clone().unwrap_or_default(),
                                }
                            } else {
                                LoserContent::LiveLocal {
                                    expected_chunks: loser_state.chunks.clone(),
                                }
                            };
                            (local.device_id, content)
                        }
                        Side::Remote => {
                            // The remote loser copy is rebuilt from blobs we
                            // may not have yet; ride the fetch list.
                            for (id, len) in &loser_state.chunks {
                                fetch_cand.insert(*id, *len);
                            }
                            (
                                remote.device_id,
                                LoserContent::FromStore {
                                    kind: loser_state.kind,
                                    exec: loser_state.exec,
                                    mtime_sec: loser_state.mtime_sec,
                                    mtime_nsec: loser_state.mtime_nsec,
                                    chunks: loser_state.chunks.clone(),
                                    target: loser_state.target.clone(),
                                },
                            )
                        }
                    };
                    quarantine.push(QuarantineOp {
                        path: path.clone(),
                        loser_device: loser_dev,
                        loser_mtime_sec: loser_state.mtime_sec,
                        loser_mtime_nsec: loser_state.mtime_nsec,
                        exec: loser_state.exec,
                        content,
                    });
                }
            }
        }
        decisions.insert(key.clone(), decision);
    }

    // Subtract what the peer provably has from the send list, and what the
    // local manifest provably references from the fetch list.
    let remote_refs = manifest_chunk_refs(store, &remote.root_tree_id)?;
    let local_refs = manifest_chunk_refs(store, &local.root_tree_id)?;
    let send: BTreeMap<BlobId, u64> = send_cand
        .into_iter()
        .filter(|(id, _)| !remote_refs.contains(id))
        .collect();
    let fetch: BTreeMap<BlobId, u64> = fetch_cand
        .into_iter()
        .filter(|(id, _)| !local_refs.contains(id))
        .collect();

    // --- structural passes ---------------------------------------------------

    let removal_paths: BTreeSet<String> = materialize
        .iter()
        .filter(|op| op.result.is_none())
        .map(|op| join(&op.path))
        .collect();

    // Paths whose content survives locally against the pure merged result:
    // local-side conflict winners, quarantine destinations, and any silent
    // local keep that carries content. Ancestor deletions above these are
    // suppressed so a remote teardown cannot eat a resurrection.
    let mut survivors: Vec<CompPath> = Vec::new();
    for q in &quarantine {
        survivors.push(q.path.clone());
    }
    for c in &conflicts {
        if c.winner == Side::Local {
            survivors.push(c.path.clone());
        }
    }
    for (key, d) in &decisions {
        if matches!(d, Decision::KeepLocal) {
            if let Some((p, lv)) = local_view.get(key) {
                if lv.side.is_some() {
                    survivors.push(p.clone());
                }
            }
        }
    }

    let mut suppressed: BTreeSet<String> = BTreeSet::new();
    for p in &survivors {
        for i in 1..p.len() {
            let anc = join(&p[..i]);
            if removal_paths.contains(&anc) {
                suppressed.insert(anc);
            }
        }
    }
    if !suppressed.is_empty() {
        materialize.retain(|op| !(op.result.is_none() && suppressed.contains(&join(&op.path))));
    }

    // Remaining checks: an upsert or quarantine file may not land beneath a
    // path another operation tears down or replaces with something that is
    // not a directory.
    for target in materialize.iter().filter(|op| op.result.is_some()).map(|op| &op.path)
        .chain(quarantine.iter().map(|q| &q.path))
    {
        for i in 1..target.len() {
            let anc = join(&target[..i]);
            if suppressed.contains(&anc) {
                continue;
            }
            let replaced_by_non_dir = materialize.iter().any(|op| {
                op.result.is_some()
                    && join(&op.path) == anc
                    && op.result.as_ref().is_some_and(|s| s.kind != EntryKind::Dir)
            });
            if replaced_by_non_dir || removal_paths.contains(&anc) {
                return Err(ReconcileError::StructuralConflict {
                    ancestor: anc,
                    path: join(target),
                });
            }
        }
    }

    conflicts.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(ActionPlan {
        materialize,
        quarantine,
        send: send.into_iter().collect(),
        fetch: fetch.into_iter().collect(),
        conflicts,
        guard_expected: Some(local.clone()),
    })
}
