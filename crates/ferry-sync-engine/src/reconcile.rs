use std::collections::{BTreeMap, BTreeSet};

use ferry_store::diff::{diff_roots, CompPath, EntryKind, EntryState};
use ferry_store::format::{BlobId, BlobKind};
use ferry_store::manifest::{
    parse_manifest, parse_tree_node, serialize_manifest, serialize_tree_node, EntryPayload,
    ManifestError, RootManifest, TreeNode,
};
use ferry_store::store::{Store, StoreError};
use thiserror::Error;

use crate::converge::Side;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConflictKind {
    BothChanged,

    DeleteVsEdit,

    AddVsAdd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MaterializeOp {
    pub(crate) path: CompPath,
    pub(crate) base: Option<EntryState>,
    pub(crate) result: Option<EntryState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoserContent {
    LiveLocal {
        expected_chunks: Vec<(BlobId, u64)>,
    },

    LiveLocalSymlink {
        expected_target: String,
    },

    FromStore {
        kind: ferry_store::diff::EntryKind,
        exec: bool,
        mtime_sec: i64,
        mtime_nsec: u32,
        chunks: Vec<(BlobId, u64)>,
        target: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuarantineOp {
    pub(crate) path: CompPath,

    pub(crate) loser_device: [u8; 32],

    pub(crate) loser_mtime_sec: i64,
    pub(crate) loser_mtime_nsec: u32,

    pub(crate) exec: bool,
    pub(crate) content: LoserContent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlannedConflict {
    pub(crate) path: CompPath,
    pub(crate) kind: ConflictKind,
    pub(crate) winner: Side,
    pub(crate) loser: Side,

    pub(crate) winner_device: [u8; 32],
    pub(crate) loser_device: [u8; 32],

    pub(crate) winner_mtime_sec: i64,
    pub(crate) winner_mtime_nsec: u32,

    pub(crate) loser_mtime_sec: Option<i64>,
    pub(crate) loser_mtime_nsec: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ActionPlan {
    pub(crate) materialize: Vec<MaterializeOp>,

    pub(crate) quarantine: Vec<QuarantineOp>,

    pub(crate) send: Vec<(BlobId, u64)>,

    pub(crate) fetch: Vec<(BlobId, u64)>,
    pub(crate) conflicts: Vec<PlannedConflict>,
    pub(crate) has_local_wins: bool,
}

pub(crate) struct ReconcileInput<'a> {
    pub(crate) store: &'a Store,
    pub(crate) local: &'a RootManifest,
    pub(crate) remote: &'a RootManifest,

    pub(crate) base: Option<&'a RootManifest>,
}

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

fn same_content(l: &EntryState, r: &EntryState) -> bool {
    l.kind == r.kind
        && match l.kind {
            EntryKind::Symlink => l.target == r.target,
            _ => l.chunks == r.chunks,
        }
}

fn pick_winner(
    l: &EntryState,
    r: &EntryState,
    local_dev: &[u8; 32],
    remote_dev: &[u8; 32],
) -> Side {
    let l_mt = (l.mtime_sec, l.mtime_nsec);
    let r_mt = (r.mtime_sec, r.mtime_nsec);
    if l_mt != r_mt {
        return if l_mt > r_mt {
            Side::Local
        } else {
            Side::Remote
        };
    }
    if local_dev > remote_dev {
        Side::Local
    } else {
        Side::Remote
    }
}

enum Decision {
    Nothing,

    ApplyRemote,

    KeepLocal,

    Conflict { kind: ConflictKind, winner: Side },
}

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

enum SafeBase<'a> {
    Proven(&'a RootManifest),

    Empty,
}

fn is_ancestor(store: &Store, ancestor: &BlobId, of: &RootManifest) -> bool {
    if *blake3::hash(&serialize_manifest(of)).as_bytes() == *ancestor {
        return true;
    }
    let mut curr_parent = of.parent_manifest_id;
    let mut depth = 0;
    while curr_parent != [0u8; 32] && depth < 256 {
        if curr_parent == *ancestor {
            return true;
        }
        match store.get(BlobKind::Manifest, &curr_parent) {
            Ok(bytes) => match parse_manifest(&bytes) {
                Ok(m) => curr_parent = m.parent_manifest_id,
                Err(_) => return false,
            },
            Err(_) => return false,
        }
        depth += 1;
    }
    false
}

fn resolve_safe_base<'a>(
    store: &Store,
    local: &'a RootManifest,
    remote: &'a RootManifest,
    base: Option<&'a RootManifest>,
) -> SafeBase<'a> {
    let Some(b) = base else {
        return SafeBase::Empty;
    };
    let base_id = *blake3::hash(&serialize_manifest(b)).as_bytes();
    if is_ancestor(store, &base_id, local) && is_ancestor(store, &base_id, remote) {
        SafeBase::Proven(b)
    } else {
        SafeBase::Empty
    }
}

pub(crate) fn reconcile(input: ReconcileInput<'_>) -> Result<ActionPlan, ReconcileError> {
    let ReconcileInput {
        store,
        local,
        remote,
        base,
    } = input;

    let safe_base = resolve_safe_base(store, local, remote, base);

    let empty_root = store.put_meta(
        BlobKind::TreeNode,
        &serialize_tree_node(&TreeNode::default()),
    )?;
    let base_root = match safe_base {
        SafeBase::Proven(m) => m.root_tree_id,
        SafeBase::Empty => empty_root,
    };

    let local_view = index_change_set(&diff_roots(store, &base_root, &local.root_tree_id)?);
    let remote_view = index_change_set(&diff_roots(store, &base_root, &remote.root_tree_id)?);

    let mut keys: BTreeSet<String> = local_view.keys().cloned().collect();
    keys.extend(remote_view.keys().cloned());

    let mut decided: Vec<(String, Decision, Option<EntryState>, Option<EntryState>)> = Vec::new();
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

        let (b, l, r) = match (lv, rv) {
            (Some((_, lv)), Some((_, rv))) => (
                lv.base.clone().or_else(|| rv.base.clone()),
                lv.side.clone(),
                rv.side.clone(),
            ),
            (Some((_, lv)), None) => (lv.base.clone(), lv.side.clone(), lv.base.clone()),
            (None, Some((_, rv))) => (rv.base.clone(), rv.base.clone(), rv.side.clone()),
            (None, None) => unreachable!("key came from one of the views"),
        };

        let local_changed = l != b;
        let remote_changed = r != b;

        let decision = match (local_changed, remote_changed) {
            (false, false) => Decision::Nothing,
            (true, false) => Decision::KeepLocal,
            (false, true) => Decision::ApplyRemote,
            (true, true) => {
                if l == r {
                    Decision::Nothing
                } else {
                    match (&l, &r) {
                        (Some(ls), Some(rs)) => {
                            if same_content(ls, rs) {
                                if pick_winner(ls, rs, &local.device_id, &remote.device_id)
                                    == Side::Local
                                {
                                    Decision::KeepLocal
                                } else {
                                    Decision::ApplyRemote
                                }
                            } else if ls.kind == EntryKind::Dir && rs.kind == EntryKind::Dir {
                                Decision::Nothing
                            } else {
                                let kind = if l.is_none() || r.is_none() {
                                    ConflictKind::DeleteVsEdit
                                } else if b.is_none() {
                                    ConflictKind::AddVsAdd
                                } else {
                                    ConflictKind::BothChanged
                                };
                                let winner = match kind {
                                    ConflictKind::DeleteVsEdit => {
                                        if l.is_some() {
                                            Side::Local
                                        } else {
                                            Side::Remote
                                        }
                                    }
                                    _ => pick_winner(ls, rs, &local.device_id, &remote.device_id),
                                };
                                Decision::Conflict { kind, winner }
                            }
                        }
                        _ => Decision::Conflict {
                            kind: ConflictKind::DeleteVsEdit,
                            winner: if l.is_some() {
                                Side::Local
                            } else {
                                Side::Remote
                            },
                        },
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
                    winner_mtime_sec: w_state.map_or(0, |s| s.mtime_sec),
                    winner_mtime_nsec: w_state.map_or(0, |s| s.mtime_nsec),
                    loser_mtime_sec: lost.as_ref().map(|s| s.mtime_sec),
                    loser_mtime_nsec: lost.as_ref().map(|s| s.mtime_nsec),
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

                if let Some(loser_state) = lost {
                    if loser_state.kind == EntryKind::Dir {
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
        decided.push((key.clone(), decision, l, r));
    }

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

    let mut final_state: BTreeMap<String, Option<&EntryState>> = BTreeMap::new();
    for (key, d, l, r) in &decided {
        let s = match d {
            Decision::Nothing => continue,
            Decision::KeepLocal => l.as_ref(),
            Decision::ApplyRemote => r.as_ref(),
            Decision::Conflict { winner, .. } => match winner {
                Side::Local => l.as_ref(),
                Side::Remote => r.as_ref(),
            },
        };
        final_state.insert(key.clone(), s);
    }

    let removal_keys: std::collections::HashSet<String> = materialize
        .iter()
        .filter(|op| op.result.is_none())
        .map(|op| join(&op.path))
        .collect();

    let mut survivors: Vec<CompPath> = Vec::new();
    for q in &quarantine {
        survivors.push(q.path.clone());
    }
    for c in &conflicts {
        if c.winner == Side::Local {
            survivors.push(c.path.clone());
        }
    }
    for (key, d, l, _) in &decided {
        if matches!(d, Decision::KeepLocal) && l.is_some() {
            if let Some((p, _)) = local_view.get(key).or_else(|| remote_view.get(key)) {
                survivors.push(p.clone());
            }
        }
    }

    let mut suppressed: BTreeSet<String> = BTreeSet::new();
    for p in &survivors {
        for i in 1..p.len() {
            let anc = join(&p[..i]);
            if removal_keys.contains(&anc) {
                suppressed.insert(anc);
            }
        }
    }
    if !suppressed.is_empty() {
        materialize.retain(|op| !(op.result.is_none() && suppressed.contains(&join(&op.path))));
    }

    let targets: Vec<CompPath> = materialize
        .iter()
        .filter(|op| op.result.is_some())
        .map(|op| op.path.clone())
        .chain(quarantine.iter().map(|q| q.path.clone()))
        .collect();
    let mut synth_dirs: BTreeMap<String, EntryState> = BTreeMap::new();
    for target in &targets {
        for i in 1..target.len() {
            let anc_key = join(&target[..i]);
            if suppressed.contains(&anc_key) || synth_dirs.contains_key(&anc_key) {
                continue;
            }
            match final_state.get(&anc_key) {
                Some(None) => {
                    let base_state = local_view
                        .get(&anc_key)
                        .and_then(|(_, v)| v.base.clone())
                        .or_else(|| remote_view.get(&anc_key).and_then(|(_, v)| v.base.clone()));
                    match base_state {
                        Some(s) if s.kind == EntryKind::Dir => {
                            synth_dirs.insert(anc_key, s);
                        }
                        _ => {
                            return Err(ReconcileError::StructuralConflict {
                                ancestor: anc_key,
                                path: join(target),
                            });
                        }
                    }
                }
                Some(Some(s)) if s.kind != EntryKind::Dir => {
                    return Err(ReconcileError::StructuralConflict {
                        ancestor: anc_key,
                        path: join(target),
                    });
                }
                _ => {}
            }
        }
    }
    for (key, state) in &synth_dirs {
        materialize.push(MaterializeOp {
            path: key.split('/').map(str::to_string).collect(),
            base: None,
            result: Some(state.clone()),
        });
    }

    conflicts.sort_by(|a, b| a.path.cmp(&b.path));

    let has_local_wins = decided.iter().any(|(_, d, _, _)| {
        matches!(
            d,
            Decision::KeepLocal
                | Decision::Conflict {
                    winner: Side::Local,
                    ..
                }
        )
    });

    Ok(ActionPlan {
        materialize,
        quarantine,
        send: send.into_iter().collect(),
        fetch: fetch.into_iter().collect(),
        conflicts,
        has_local_wins,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::path::Path;

    use ferry_store::snapshot::SnapshotOutput;

    const DEV_A: [u8; 32] = [0xA1; 32];
    const DEV_B: [u8; 32] = [0xB2; 32];

    struct Pair {
        a: Device,
        b: Device,
        sa: SnapshotOutput,
        sb: SnapshotOutput,
    }

    fn pair(build_a: &dyn Fn(&Path), build_b: &dyn Fn(&Path)) -> Pair {
        let poly = poly_of(77);
        let mut a = Device::new(1, DEV_A, poly);
        let mut b = Device::new(2, DEV_B, poly);
        build_a(&a.tree);
        build_b(&b.tree);
        let sa = a.snapshot();
        let sb = b.snapshot();

        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);
        Pair { a, b, sa, sb }
    }

    fn plan_on_a(p: &Pair) -> ActionPlan {
        reconcile(ReconcileInput {
            store: &p.a.store,
            local: &p.sa.manifest,
            remote: &p.sb.manifest,
            base: None,
        })
        .unwrap()
    }

    #[test]
    fn base_less_disjoint_trees_union_with_no_conflicts() {
        let p = pair(
            &|t| write_file(&t.join("only-a.txt"), b"AAA", false, (100, 0)),
            &|t| write_file(&t.join("only-b.txt"), b"BBB", false, (200, 0)),
        );
        let plan = plan_on_a(&p);
        assert!(plan.conflicts.is_empty());
        assert!(plan.quarantine.is_empty());

        assert_eq!(plan.materialize.len(), 1);
        assert_eq!(
            join(&plan.materialize[0].path),
            "only-b.txt",
            "remote-only addition is materialized locally"
        );
        assert_eq!(
            plan.send.len(),
            1,
            "A's new chunk is the only thing B lacks"
        );
        assert_eq!(
            plan.fetch.len(),
            1,
            "B's chunk must be fetched before A can apply it"
        );
    }

    #[test]
    fn identical_base_less_trees_produce_zero_ops() {
        let build = |t: &Path| {
            write_file(&t.join("same.txt"), b"identical bytes", true, (555, 7));
            std::fs::create_dir(t.join("nested")).unwrap();
            write_file(&t.join("nested/deep.txt"), b"deep", false, (556, 8));
            set_dir_mtime(&t.join("nested"), 555, 9);
        };
        let p = pair(&build, &build);
        let plan = plan_on_a(&p);
        assert!(
            plan.materialize.is_empty()
                && plan.quarantine.is_empty()
                && plan.send.is_empty()
                && plan.fetch.is_empty()
                && plan.conflicts.is_empty(),
            "same content, same metadata → nothing anywhere (got {plan:?})"
        );
    }

    #[test]
    fn add_vs_add_identical_content_differing_mtimes_resolves_silently() {
        let p = pair(
            &|t| write_file(&t.join("f.txt"), b"one content", false, (10, 0)),
            &|t| write_file(&t.join("f.txt"), b"one content", false, (20, 0)),
        );
        let plan = plan_on_a(&p);
        assert!(plan.conflicts.is_empty() && plan.quarantine.is_empty());

        assert_eq!(plan.materialize.len(), 1);
        assert_eq!(plan.materialize[0].result.as_ref().unwrap().mtime_sec, 20);
    }

    #[test]
    fn both_changed_newer_side_wins_and_loser_is_quarantined() {
        let p = pair(
            &|t| write_file(&t.join("f.txt"), b"local version", false, (300, 0)),
            &|t| write_file(&t.join("f.txt"), b"remote older", false, (200, 0)),
        );
        let plan = plan_on_a(&p);

        assert_eq!(plan.conflicts.len(), 1);
        let c = &plan.conflicts[0];
        assert_eq!(c.winner, Side::Local, "A is newer and wins");
        assert_eq!(c.kind, ConflictKind::AddVsAdd, "no base existed");
        assert_eq!(plan.quarantine.len(), 1);
        match &plan.quarantine[0].content {
            LoserContent::FromStore { chunks, .. } => assert_eq!(chunks.len(), 1),
            other => panic!("remote loser must come from the store, got {other:?}"),
        }
        assert!(plan.materialize.is_empty(), "winner is live already");
        assert!(!plan.send.is_empty(), "B needs A's winner bytes");
        assert!(
            !plan.fetch.is_empty(),
            "quarantining B's loser needs its blob"
        );
    }

    #[test]
    fn exact_mtime_tie_breaks_on_higher_device_id() {
        let p = pair(
            &|t| write_file(&t.join("tie.txt"), b"from A", false, (42, 42)),
            &|t| write_file(&t.join("tie.txt"), b"from B", false, (42, 42)),
        );
        let plan = plan_on_a(&p);
        assert_eq!(
            plan.conflicts[0].winner,
            Side::Remote,
            "tie: DEV_B is the higher device id"
        );

        let mirrored = reconcile(ReconcileInput {
            store: &p.b.store,
            local: &p.sb.manifest,
            remote: &p.sa.manifest,
            base: None,
        })
        .unwrap();
        assert_eq!(
            mirrored.conflicts[0].winner,
            Side::Local,
            "B's local copy (DEV_B) wins on B too"
        );
    }

    #[test]
    fn delete_vs_edit_resurrects_the_edit() {
        let mut p = pair(
            &|t| write_file(&t.join("f.txt"), b"base", false, (50, 0)),
            &|t| write_file(&t.join("f.txt"), b"base", false, (50, 0)),
        );
        let base = p.sa.manifest.clone();

        p.b.parent = p.sa.manifest_id;

        write_file(&p.a.tree.join("f.txt"), b"the edit", false, (60, 0));
        std::fs::remove_file(p.b.tree.join("f.txt")).unwrap();
        let sa = p.a.snapshot();
        let sb = p.b.snapshot();
        transfer_manifest(&p.a.store, &p.b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&p.b.store, &p.a.store, &sb.manifest, sb.manifest_id);

        let plan = reconcile(ReconcileInput {
            store: &p.a.store,
            local: &sa.manifest,
            remote: &sb.manifest,
            base: Some(&base),
        })
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(plan.conflicts[0].kind, ConflictKind::DeleteVsEdit);
        assert_eq!(
            plan.conflicts[0].winner,
            Side::Local,
            "the edit beats the deletion"
        );
        assert!(
            plan.quarantine.is_empty(),
            "nothing to save; deletion has no bytes"
        );
        assert!(plan.materialize.is_empty(), "edit stays live on the editor");
    }

    #[test]
    fn edit_vs_delete_resurrection_is_materialized_on_the_deleter() {
        let mut p = pair(
            &|t| write_file(&t.join("f.txt"), b"base", false, (50, 0)),
            &|t| write_file(&t.join("f.txt"), b"base", false, (50, 0)),
        );
        let base = p.sa.manifest.clone();
        p.b.parent = p.sa.manifest_id;

        std::fs::remove_file(p.a.tree.join("f.txt")).unwrap();
        write_file(&p.b.tree.join("f.txt"), b"edited elsewhere", false, (70, 0));
        let sa = p.a.snapshot();
        let sb = p.b.snapshot();
        transfer_manifest(&p.a.store, &p.b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&p.b.store, &p.a.store, &sb.manifest, sb.manifest_id);

        let plan = reconcile(ReconcileInput {
            store: &p.a.store,
            local: &sa.manifest,
            remote: &sb.manifest,
            base: Some(&base),
        })
        .unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(
            plan.conflicts[0].winner,
            Side::Remote,
            "B edited, A deleted"
        );
        assert_eq!(plan.materialize.len(), 1, "A resurrects B's edit locally");
        assert!(plan.quarantine.is_empty());
    }

    #[test]
    fn structural_conflict_refuses_dir_replacement_against_nested_edit() {
        let mut p = pair(
            &|t| {
                write_file(&t.join("d/inner.txt"), b"base inner", false, (50, 0));
                set_dir_mtime(&t.join("d"), 49, 0);
            },
            &|t| {
                write_file(&t.join("d/inner.txt"), b"base inner", false, (50, 0));
                set_dir_mtime(&t.join("d"), 49, 0);
            },
        );
        let _ = &p.sa;
        let base = p.sa.manifest.clone();
        p.b.parent = p.sa.manifest_id;

        std::fs::remove_dir_all(p.a.tree.join("d")).unwrap();
        write_file(&p.a.tree.join("d"), b"now a file", false, (80, 0));

        write_file(
            &p.b.tree.join("d/inner.txt"),
            b"edited inner",
            false,
            (81, 0),
        );
        let sa = p.a.snapshot();
        let sb = p.b.snapshot();
        transfer_manifest(&p.a.store, &p.b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&p.b.store, &p.a.store, &sb.manifest, sb.manifest_id);

        let err = reconcile(ReconcileInput {
            store: &p.a.store,
            local: &sa.manifest,
            remote: &sb.manifest,
            base: Some(&base),
        })
        .unwrap_err();
        assert!(
            matches!(err, ReconcileError::StructuralConflict { .. }),
            "{err}"
        );
    }
}
