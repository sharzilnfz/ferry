use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use ferry_materialize::temp::{fresh_entropy, temp_name_for, TempStyle};
use ferry_materialize::{
    Applier, ApplyStats, DivergeReason, Divergence, MaterializeError, Overwrite,
};
use ferry_platform::time as timefmt;
use ferry_store::agreement::{AgreedRecord, AgreementError, AgreementLedger};
use ferry_store::diff::{join_path, ChangeSet, EntryKind, EntryState};
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::manifest::{serialize_manifest, RootManifest};
use ferry_store::store::{Store, StoreError};
use thiserror::Error;

use crate::held::{HeldChunk, HeldEntry, HeldLedger};
use crate::pin::PinStore;
use crate::reconcile::{
    reconcile, ActionPlan, ConflictKind, LoserContent, QuarantineOp, ReconcileError, ReconcileInput,
};
use crate::report::{append_entries, ConflictEntry, DeviceStamp, LogError};

const MAX_LANDING_ATTEMPTS: u32 = 128;

#[derive(Debug, Error)]
pub enum ConvergenceError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("reconciliation failed: {0}")]
    Reconcile(#[from] ReconcileError),
    #[error("manifest decode failed: {0}")]
    Manifest(#[from] ferry_store::manifest::ManifestError),
    #[error("materialization failed: {0}")]
    Materialize(#[from] MaterializeError),
    #[error("conflict report failed: {0}")]
    Log(#[from] LogError),
    #[error("agreement ledger failed: {0}")]
    Agreement(#[from] AgreementError),
    #[error("pin error: {0}")]
    Pin(#[from] crate::pin_error::PinError),
    #[error("io failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "cannot converge safely under this pin: pinned path {pinned} sits inside \
         change path {other}; one half would move an ancestor of the other. Widen or \
         narrow the pin scope so pinned and unpinned changes do not nest."
    )]
    StructuralSplit { pinned: String, other: String },
    #[error("{count} required data blob(s) are missing from the store after fetch")]
    MissingBlobs { count: usize },
    #[error("blob fetch failed: {0}")]
    Fetch(String),
}

fn io_at(path: impl Into<PathBuf>, e: std::io::Error) -> ConvergenceError {
    ConvergenceError::Io {
        path: path.into(),
        source: e,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldDecision {
    RemoteApply,

    RemoteDelete,

    Conflict { winner: Option<Side> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldPath {
    pub path: String,
    pub decision: HeldDecision,

    pub chunks: Vec<(BlobId, u64)>,
}

#[derive(Clone, Copy, Debug)]
pub struct LocalTree<'a> {
    pub root: &'a Path,

    pub manifest: &'a RootManifest,
}

#[derive(Clone, Debug, Default)]
pub struct ConvergenceResult {
    pub apply: ApplyStats,

    pub quarantined: Vec<String>,

    pub conflicts: Vec<ConflictEntry>,

    pub send: Vec<(BlobId, u64)>,

    pub held: Vec<HeldPath>,

    pub agreed_manifest_id: Option<BlobId>,
    pub has_local_wins: bool,
}

impl ConvergenceResult {
    pub fn is_noop(&self) -> bool {
        self.apply.mutations() == 0
            && self.quarantined.is_empty()
            && self.conflicts.is_empty()
            && self.send.is_empty()
            && self.held.is_empty()
    }
}

pub trait BlobFetch {
    fn fetch(&mut self, want: &[(BlobId, u64)]) -> Result<(), ConvergenceError>;
}

type HoldGate<'a> = Box<dyn Fn(&[String]) -> bool + 'a>;

enum HoldConfig<'a> {
    Auto,
    Disabled,
    Custom(HoldGate<'a>),
}

pub struct ConvergenceEngine<'a> {
    store: &'a Store,
    root: &'a Path,
    state_dir: Option<&'a Path>,
    now: (i64, u32),
    hold: HoldConfig<'a>,
    fetcher: Option<&'a mut dyn BlobFetch>,
}

impl<'a> ConvergenceEngine<'a> {
    pub fn new(store: &'a Store, root: &'a Path) -> Self {
        ConvergenceEngine {
            store,
            root,
            state_dir: None,
            now: timefmt::now_unix(),
            hold: HoldConfig::Auto,
            fetcher: None,
        }
    }

    pub fn state_dir(mut self, dir: &'a Path) -> Self {
        self.state_dir = Some(dir);
        self
    }

    pub fn at(mut self, now: (i64, u32)) -> Self {
        self.now = now;
        self
    }

    pub fn hold(mut self, gate: impl Fn(&[String]) -> bool + 'a) -> Self {
        self.hold = HoldConfig::Custom(Box::new(gate));
        self
    }

    pub fn no_hold(mut self) -> Self {
        self.hold = HoldConfig::Disabled;
        self
    }

    pub fn fetch_with(mut self, fetcher: &'a mut dyn BlobFetch) -> Self {
        self.fetcher = Some(fetcher);
        self
    }

    pub fn converge(
        &mut self,
        local: &RootManifest,
        remote: &RootManifest,
        base: Option<&RootManifest>,
    ) -> Result<ConvergenceResult, ConvergenceError> {
        let plan = reconcile(ReconcileInput {
            store: self.store,
            local,
            remote,
            base,
        })?;

        let state_dir = self.state_dir.unwrap_or_else(|| self.store.store_dir());
        let (plan, held) = match &self.hold {
            HoldConfig::Custom(gate) => gate_plan(plan, gate.as_ref())?,
            HoldConfig::Disabled => (plan, Vec::new()),
            HoldConfig::Auto => {
                let rec = PinStore::new(state_dir).load()?;
                if let Some(rec) = rec {
                    if rec.holding() {
                        let gate: Box<dyn Fn(&[String]) -> bool> =
                            if rec.paths.iter().any(|p| p == "*") {
                                Box::new(|_: &[String]| true)
                            } else {
                                let mut builder = ignore::gitignore::GitignoreBuilder::new("");
                                for line in &rec.paths {
                                    builder.add_line(None, line).map_err(|e| {
                                        crate::pin_error::PinError::BadPattern {
                                            line: line.clone(),
                                            reason: e.to_string(),
                                        }
                                    })?;
                                }
                                let gi = builder.build().map_err(|e| {
                                    crate::pin_error::PinError::BadPattern {
                                        line: rec.paths.join(", "),
                                        reason: e.to_string(),
                                    }
                                })?;
                                let patterns = rec.paths.clone();
                                Box::new(move |rel: &[String]| {
                                    if matches!(
                                        gi.matched_path_or_any_parents(
                                            std::path::Path::new(&rel.join("/")),
                                            false
                                        ),
                                        ignore::Match::Ignore(_)
                                    ) {
                                        return true;
                                    }
                                    let joined = rel.join("/");
                                    patterns.iter().any(|pat| {
                                        let clean_pat = pat.trim_start_matches('/');
                                        clean_pat.starts_with(&format!("{joined}/"))
                                    })
                                })
                            };
                        gate_plan(plan, gate.as_ref())?
                    } else {
                        (plan, Vec::new())
                    }
                } else {
                    (plan, Vec::new())
                }
            }
        };

        let missing: Vec<(BlobId, u64)> = plan
            .fetch
            .iter()
            .filter(|(id, _)| self.store.get(BlobKind::DataChunk, id).is_err())
            .copied()
            .collect();
        if !missing.is_empty() {
            match self.fetcher.as_deref_mut() {
                Some(f) => f.fetch(&missing)?,
                None => {
                    return Err(ConvergenceError::MissingBlobs {
                        count: missing.len(),
                    })
                }
            }
            let leftover = missing
                .iter()
                .filter(|(id, _)| self.store.get(BlobKind::DataChunk, id).is_err())
                .count();
            if leftover > 0 {
                return Err(ConvergenceError::MissingBlobs { count: leftover });
            }
        }

        let mut dest_rel: Vec<String> = Vec::with_capacity(plan.quarantine.len());
        let mut dest_abs: Vec<PathBuf> = Vec::with_capacity(plan.quarantine.len());
        let mut buffers: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        for (idx, op) in plan.quarantine.iter().enumerate() {
            let name = op.path.last().expect("stored paths are non-empty").clone();
            let parent = &op.path[..op.path.len() - 1];
            let candidate =
                crate::naming::conflict_display_name(&name, &op.loser_device, op.loser_mtime_sec);
            let abs = crate::naming::unique_conflict_dest(self.root, parent, &candidate)
                .map_err(|e| io_at(self.root.join(&candidate), e))?;
            if let LoserContent::LiveLocal { expected_chunks } = &op.content {
                let live_abs = ferry_materialize::resolve_live(self.root, &op.path);
                buffers.insert(
                    idx,
                    read_live_verified(&live_abs, &op.path, expected_chunks)?,
                );
            }
            dest_rel.push(join_path(&rel_under(self.root, &abs)));
            dest_abs.push(abs);
        }
        for (idx, op) in plan.quarantine.iter().enumerate() {
            let dest_dir = dest_abs[idx]
                .parent()
                .expect("dest always has a parent")
                .to_path_buf();
            let candidate_base = dest_abs[idx]
                .file_name()
                .expect("dest is never the root")
                .to_string_lossy()
                .into_owned();
            let landed = write_loser_copy(
                self.store,
                self.root,
                op,
                &dest_dir,
                &candidate_base,
                buffers.get(&idx).map(Vec::as_slice),
            )?;
            dest_rel[idx] = join_path(&rel_under(self.root, &landed));
            dest_abs[idx] = landed;
        }

        let mut cs = ChangeSet::default();
        for op in &plan.materialize {
            fold_into_change_set(op.base.as_ref(), op.result.as_ref(), &op.path, &mut cs);
        }
        let apply = if cs.is_empty() {
            ApplyStats::default()
        } else {
            Applier::new(self.store, self.root)
                .overwrite(Overwrite::Expect {
                    expected: local.clone(),
                })
                .apply_change_set(&cs)?
        };

        let folder_id = hex(&local.folder_id);
        let mut conflicts: Vec<ConflictEntry> = Vec::new();
        for c in &plan.conflicts {
            conflicts.push(ConflictEntry {
                ts: timefmt::fmt_rfc3339(self.now.0),
                folder_id: folder_id.clone(),
                path: join_path(&c.path),
                kind: kind_str(c.kind).to_string(),
                winner: DeviceStamp::new(
                    c.winner_device,
                    Some((c.winner_mtime_sec, c.winner_mtime_nsec)),
                ),
                loser: DeviceStamp::new(
                    c.loser_device,
                    c.loser_mtime_sec
                        .map(|s| (s, c.loser_mtime_nsec.unwrap_or(0))),
                ),
                quarantined_as: plan
                    .quarantine
                    .iter()
                    .position(|op| op.path == c.path)
                    .and_then(|pos| dest_rel.get(pos).cloned()),
            });
        }
        if let Some(sd) = self.state_dir.or(Some(self.store.store_dir())) {
            append_entries(sd, &conflicts)?;
        }
        // 5. Append held ledger if any entries were held.
        if !held.is_empty() {
            let peer_hex = hex(&remote.device_id);
            let remote_bytes = serialize_manifest(remote);
            let remote_id = *blake3::hash(&remote_bytes).as_bytes();
            let remote_hex = hex(&remote_id);
            self.store.put_meta(BlobKind::Manifest, &remote_bytes)?;
            self.store.flush()?;
            let entries: Vec<HeldEntry> = held
                .iter()
                .map(|h| HeldEntry {
                    held_sec: self.now.0,
                    held_nsec: self.now.1,
                    path: h.path.clone(),
                    device_id: peer_hex.clone(),
                    remote_manifest_id: remote_hex.clone(),
                    chunks: h
                        .chunks
                        .iter()
                        .map(|(id, len)| HeldChunk {
                            id: hex(id),
                            len: *len,
                        })
                        .collect(),
                    decision: match h.decision {
                        HeldDecision::RemoteApply => "remote_apply".to_string(),
                        HeldDecision::RemoteDelete => "remote_delete".to_string(),
                        HeldDecision::Conflict { .. } => "conflict".to_string(),
                    },
                    conflict_winner: match h.decision {
                        HeldDecision::Conflict {
                            winner: Some(Side::Local),
                        } => Some("local".to_string()),
                        HeldDecision::Conflict {
                            winner: Some(Side::Remote),
                        } => Some("remote".to_string()),
                        _ => None,
                    },
                })
                .collect();
            let ledger = HeldLedger::new(state_dir);
            let known: BTreeSet<(String, String)> = ledger
                .load_peer(&peer_hex)?
                .into_iter()
                .map(|e| (e.path, e.remote_manifest_id))
                .collect();
            let fresh: Vec<HeldEntry> = entries
                .into_iter()
                .filter(|e| !known.contains(&(e.path.clone(), e.remote_manifest_id.clone())))
                .collect();
            ledger.append(&peer_hex, &fresh)?;
        }

        let mut agreed_manifest_id = None;
        if plan.conflicts.is_empty() && held.is_empty() && plan.send.is_empty() && !plan.has_local_wins {
            let bytes = serialize_manifest(remote);
            let id = self.store.put_meta(BlobKind::Manifest, &bytes)?;
            AgreementLedger::new(self.store.store_dir()).record(
                &local.folder_id,
                &AgreedRecord {
                    peer_device_id: remote.device_id,
                    manifest_id: id,
                    agreed_sec: self.now.0,
                    agreed_nsec: self.now.1,
                },
            )?;
            agreed_manifest_id = Some(id);
        }

        Ok(ConvergenceResult {
            apply,
            quarantined: dest_rel,
            conflicts,
            send: plan.send,
            held,
            agreed_manifest_id,
            has_local_wins: plan.has_local_wins,
        })
    }
}

pub fn converge(
    local_tree: LocalTree<'_>,
    remote_manifest: &RootManifest,
    base_manifest: Option<&RootManifest>,
    store: &Store,
) -> Result<ConvergenceResult, ConvergenceError> {
    ConvergenceEngine::new(store, local_tree.root)
        .state_dir(store.store_dir())
        .converge(local_tree.manifest, remote_manifest, base_manifest)
}

fn gate_plan(
    plan: ActionPlan,
    holds: impl Fn(&[String]) -> bool,
) -> Result<(ActionPlan, Vec<HeldPath>), ConvergenceError> {
    let held_mat: Vec<bool> = plan.materialize.iter().map(|op| holds(&op.path)).collect();
    let held_qtn: Vec<bool> = plan.quarantine.iter().map(|op| holds(&op.path)).collect();
    let held_con: Vec<bool> = plan.conflicts.iter().map(|c| holds(&c.path)).collect();

    let any_held =
        held_mat.iter().any(|b| *b) || held_qtn.iter().any(|b| *b) || held_con.iter().any(|b| *b);
    if !any_held {
        return Ok((plan, Vec::new()));
    }

    let mut held_keys: BTreeSet<String> = BTreeSet::new();
    for (op, h) in plan.materialize.iter().zip(&held_mat) {
        if *h {
            held_keys.insert(join_path(&op.path));
        }
    }
    for (op, h) in plan.quarantine.iter().zip(&held_qtn) {
        if *h {
            held_keys.insert(join_path(&op.path));
        }
    }
    for (c, h) in plan.conflicts.iter().zip(&held_con) {
        if *h {
            held_keys.insert(join_path(&c.path));
        }
    }

    let other_keys: Vec<String> = [
        plan.materialize
            .iter()
            .zip(&held_mat)
            .filter(|(_, h)| !**h)
            .map(|(op, _)| join_path(&op.path))
            .collect::<Vec<_>>(),
        plan.quarantine
            .iter()
            .zip(&held_qtn)
            .filter(|(_, h)| !**h)
            .map(|(op, _)| join_path(&op.path))
            .collect(),
        plan.conflicts
            .iter()
            .zip(&held_con)
            .filter(|(_, h)| !**h)
            .map(|(c, _)| join_path(&c.path))
            .collect(),
    ]
    .concat();

    for p in &held_keys {
        for q in &other_keys {
            if nests(p, q) || nests(q, p) {
                return Err(ConvergenceError::StructuralSplit {
                    pinned: p.clone(),
                    other: q.clone(),
                });
            }
        }
    }

    let mut held = Vec::new();
    for key in &held_keys {
        let mat = plan
            .materialize
            .iter()
            .zip(&held_mat)
            .find(|(op, h)| **h && join_path(&op.path) == *key)
            .map(|(op, _)| op);
        let qtn = plan
            .quarantine
            .iter()
            .zip(&held_qtn)
            .find(|(op, h)| **h && join_path(&op.path) == *key)
            .map(|(op, _)| op);
        let con = plan
            .conflicts
            .iter()
            .zip(&held_con)
            .find(|(c, h)| **h && join_path(&c.path) == *key)
            .map(|(c, _)| c);

        let decision = match con {
            Some(c) => HeldDecision::Conflict {
                winner: Some(c.winner),
            },
            None => match mat.map(|m| &m.result) {
                Some(Some(_)) => HeldDecision::RemoteApply,
                Some(None) => HeldDecision::RemoteDelete,
                None => HeldDecision::Conflict { winner: None },
            },
        };

        let mut chunks: Vec<(BlobId, u64)> = Vec::new();
        match mat.and_then(|m| m.result.as_ref()) {
            Some(state) => chunks.extend(state.chunks.iter().copied()),
            None => {
                if let Some(LoserContent::FromStore { chunks: cs, .. }) = qtn.map(|q| &q.content) {
                    chunks.extend(cs.iter().copied());
                }
            }
        }

        held.push(HeldPath {
            path: key.clone(),
            decision,
            chunks,
        });
    }

    // Held chunks are not withheld from `send`: a chunk may be shared between a
    // held path and a non-held path (deduplicated by BlobId). Without the full
    // chunk→path map (removed with `chunk_path_map` DFS), we cannot prove a
    // chunk is held-only. Withholding by flat_map would starve non-held files
    // that share the chunk. The store fetch dedups on the remote side, so
    // keeping all send entries is correct and preserves the single-BFS budget.
    let send = plan.send.clone();

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
        fetch: plan.fetch.clone(),
        conflicts: plan
            .conflicts
            .iter()
            .zip(&held_con)
            .filter(|(_, h)| !**h)
            .map(|(c, _)| c.clone())
            .collect(),
        has_local_wins: plan.has_local_wins,
    };

    Ok((apply, held))
}

fn nests(prefix: &str, whole: &str) -> bool {
    whole.len() > prefix.len()
        && whole.starts_with(prefix)
        && whole[prefix.len()..].starts_with('/')
}

fn kind_str(k: ConflictKind) -> &'static str {
    match k {
        ConflictKind::BothChanged => "both_changed",
        ConflictKind::DeleteVsEdit => "delete_vs_edit",
        ConflictKind::AddVsAdd => "add_vs_add",
    }
}

fn rel_under(root: &Path, abs: &Path) -> Vec<String> {
    abs.strip_prefix(root)
        .unwrap_or(Path::new(""))
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect()
}

fn read_live_verified(
    abs: &Path,
    rel: &[String],
    expected_chunks: &[(BlobId, u64)],
) -> Result<Vec<u8>, ConvergenceError> {
    let mut f = std::fs::File::open(abs).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => diverged(rel, DivergeReason::ExpectedPresent),
        _ => io_at(abs, e),
    })?;
    let declared_total: u64 = expected_chunks.iter().map(|c| c.1).sum();
    let mut out = Vec::with_capacity(declared_total as usize);
    for (id, len) in expected_chunks {
        let mut region = vec![0u8; *len as usize];
        if f.read_exact(&mut region).is_err() {
            return Err(diverged(rel, DivergeReason::ContentMismatch));
        }
        if blake3::hash(&region).as_bytes() != id {
            return Err(diverged(rel, DivergeReason::ContentMismatch));
        }
        out.extend_from_slice(&region);
    }
    let mut trailing = [0u8; 1];
    if f.read(&mut trailing).unwrap_or(1) > 0 {
        return Err(diverged(
            rel,
            DivergeReason::SizeMismatch {
                expected: declared_total,
                found: declared_total + 1,
            },
        ));
    }
    Ok(out)
}

fn diverged(rel: &[String], reason: DivergeReason) -> ConvergenceError {
    ConvergenceError::Materialize(MaterializeError::Diverged {
        paths: vec![Divergence {
            path: rel.to_vec(),
            reason,
        }],
    })
}

fn write_loser_copy(
    store: &Store,
    root: &Path,
    op: &QuarantineOp,
    dest_dir: &Path,
    candidate_base: &str,
    live_bytes: Option<&[u8]>,
) -> Result<PathBuf, ConvergenceError> {
    std::fs::create_dir_all(dest_dir).map_err(|e| io_at(dest_dir, e))?;
    let display = join_path(&op.path);
    for attempt in 1..=MAX_LANDING_ATTEMPTS {
        let name = if attempt == 1 {
            candidate_base.to_string()
        } else {
            format!("{candidate_base}-{attempt}")
        };
        let dest = dest_dir.join(&name);
        let tmp = dest_dir.join(temp_name_for(
            &display,
            TempStyle::current(),
            &fresh_entropy(),
        ));
        if let Err(e) = build_tmp(store, root, op, &tmp, live_bytes) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        match rename_exclusive(&tmp, &dest) {
            Ok(()) => return Ok(dest),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&tmp);
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(io_at(&dest, e));
            }
        }
    }
    Err(io_at(
        dest_dir.join(candidate_base),
        std::io::Error::other(format!(
            "quarantine landing exhausted {MAX_LANDING_ATTEMPTS} exclusive attempts"
        )),
    ))
}

fn build_tmp(
    store: &Store,
    root: &Path,
    op: &QuarantineOp,
    tmp: &Path,
    live_bytes: Option<&[u8]>,
) -> Result<(), ConvergenceError> {
    match &op.content {
        LoserContent::LiveLocalSymlink { expected_target } => {
            let actual = std::fs::read_link(ferry_materialize::resolve_live(root, &op.path))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            if &actual != expected_target {
                return Err(diverged(
                    &op.path,
                    DivergeReason::TargetMismatch {
                        expected: expected_target.clone(),
                        found: actual,
                    },
                ));
            }
            make_symlink(expected_target, tmp)
        }
        LoserContent::LiveLocal { .. } => {
            let bytes = live_bytes.expect("pre-verified buffer must exist");
            write_bytes_with_meta(tmp, bytes, op.exec, op.loser_mtime_sec, op.loser_mtime_nsec)
        }
        LoserContent::FromStore {
            kind,
            chunks,
            target,
            ..
        } => {
            if *kind == EntryKind::Symlink {
                let t = target.clone().unwrap_or_default();
                make_symlink(&t, tmp)
            } else {
                let mut bytes = Vec::with_capacity(chunks.iter().map(|c| c.1 as usize).sum());
                for (id, len) in chunks {
                    let piece = store
                        .get(BlobKind::DataChunk, id)
                        .map_err(ConvergenceError::from)?;
                    if piece.len() as u64 != *len {
                        return Err(ConvergenceError::Materialize(
                            MaterializeError::ChunkCorrupt {
                                path: join_path(&op.path),
                                index: usize::MAX,
                                expected: format!("len {len}"),
                                found: format!("len {}", piece.len()),
                            },
                        ));
                    }
                    bytes.extend_from_slice(&piece);
                }
                write_bytes_with_meta(
                    tmp,
                    &bytes,
                    op.exec,
                    op.loser_mtime_sec,
                    op.loser_mtime_nsec,
                )
            }
        }
    }
}

fn rename_exclusive(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::hard_link(tmp, dest)?;

        let _ = std::fs::remove_file(tmp);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dest)?,
        );
        match std::fs::rename(tmp, dest) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(dest);
                Err(e)
            }
        }
    }
}

fn make_symlink(target: &str, at: &Path) -> Result<(), ConvergenceError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at).map_err(|e| io_at(at, e))
    }
    #[cfg(windows)]
    {
        let resolved = at
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(target);
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, at)
        } else {
            std::os::windows::fs::symlink_file(target, at)
        }
        .map_err(|e| io_at(at, e))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, at);
        Err(ConvergenceError::Io {
            path: PathBuf::new(),
            source: std::io::Error::other("symlinks unsupported"),
        })
    }
}

fn write_bytes_with_meta(
    tmp: &Path,
    bytes: &[u8],
    exec: bool,
    sec: i64,
    nsec: u32,
) -> Result<(), ConvergenceError> {
    use std::io::Write;

    #[cfg(not(unix))]
    let _ = exec;
    {
        let mut f = std::fs::File::create(tmp).map_err(|e| io_at(tmp, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(if exec {
                0o755
            } else {
                0o644
            }))
            .map_err(|e| io_at(tmp, e))?;
        }
        f.write_all(bytes).map_err(|e| io_at(tmp, e))?;
        f.set_times(std::fs::FileTimes::new().set_modified(timefmt::join_unix(sec, nsec)))
            .map_err(|e| io_at(tmp, e))?;
        f.sync_all().map_err(|e| io_at(tmp, e))?;
    }
    Ok(())
}

fn fold_into_change_set(
    base: Option<&EntryState>,
    result: Option<&EntryState>,
    path: &[String],
    cs: &mut ChangeSet,
) {
    match (base.cloned(), result.cloned()) {
        (_, None) => {
            let state = base.cloned().unwrap_or(EntryState {
                kind: EntryKind::File,
                exec: false,
                mtime_sec: 0,
                mtime_nsec: 0,
                chunks: Vec::new(),
                target: None,
            });
            cs.removed.push(ferry_store::diff::Removed {
                path: path.to_vec(),
                state,
            });
        }
        (None, Some(r)) => cs.added.push(ferry_store::diff::Added {
            path: path.to_vec(),
            state: r,
        }),
        (Some(b), Some(r)) => {
            let m = ferry_store::diff::Modified {
                path: path.to_vec(),
                before: b.clone(),
                after: r.clone(),
            };
            if b.kind != r.kind {
                cs.type_changed.push(m);
            } else if b.chunks != r.chunks || b.target != r.target {
                cs.content_modified.push(m);
            } else {
                cs.metadata_modified.push(m);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::naming;
    use crate::reconcile::reconcile;
    use crate::report::list_conflicts;
    use crate::testutil::*;

    const DEV_A: [u8; 32] = [0xA1; 32];
    const DEV_B: [u8; 32] = [0xB2; 32];
    const NOW: (i64, u32) = (1_787_574_896, 0);

    struct PeerFetch<'x> {
        from: &'x Store,
        to: &'x Store,
    }

    impl BlobFetch for PeerFetch<'_> {
        fn fetch(&mut self, want: &[(BlobId, u64)]) -> Result<(), ConvergenceError> {
            for (id, _) in want {
                if self.to.get(BlobKind::DataChunk, id).is_err() {
                    let bytes = self
                        .from
                        .get(BlobKind::DataChunk, id)
                        .map_err(|_| ConvergenceError::MissingBlobs { count: 1 })?;
                    self.to
                        .put_blob(BlobKind::DataChunk, &bytes)
                        .map_err(ConvergenceError::from)?;
                }
            }
            Ok(())
        }
    }

    struct Rig {
        a: Device,
        b: Device,
        local: RootManifest,
        remote: RootManifest,
        base: RootManifest,
    }

    fn rig() -> Rig {
        let mut a = Device::new(1, DEV_A, poly_of(5));
        let mut b = Device::new(2, DEV_B, poly_of(5));
        write_file(&a.tree.join("f.txt"), b"base", false, (100, 0));
        write_file(&b.tree.join("f.txt"), b"base", false, (100, 0));
        let s0a = a.snapshot();
        let _s0b = b.snapshot();

        transfer_manifest(&a.store, &b.store, &s0a.manifest, s0a.manifest_id);
        b.parent = s0a.manifest_id;

        write_file(&a.tree.join("f.txt"), b"winner from A", false, (200, 0));
        write_file(&b.tree.join("f.txt"), b"loser on B", false, (150, 0));
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);

        Rig {
            a,
            b,
            local: sb.manifest,
            remote: sa.manifest,
            base: s0a.manifest,
        }
    }

    fn converge_on_b(rig: &Rig, now: (i64, u32)) -> Result<ConvergenceResult, ConvergenceError> {
        let mut fetch = PeerFetch {
            from: &rig.a.store,
            to: &rig.b.store,
        };
        let result = ConvergenceEngine::new(&rig.b.store, &rig.b.tree)
            .state_dir(&rig.b.state_dir)
            .at(now)
            .fetch_with(&mut fetch)
            .converge(&rig.local, &rig.remote, Some(&rig.base));
        result
    }

    #[test]
    fn local_loser_saved_then_overwritten_by_winner() {
        let rig = rig();
        let result = converge_on_b(&rig, NOW).unwrap();

        assert_eq!(
            std::fs::read(rig.b.tree.join("f.txt")).unwrap(),
            b"winner from A"
        );
        assert_eq!(result.quarantined.len(), 1);
        let q = &result.quarantined[0];
        assert_eq!(
            q, "f.txt.ferry-conflict.b2b2b2b2-19700101-000230",
            "name carries LOSER device short id + loser mtime UTC"
        );
        assert_eq!(std::fs::read(rig.b.tree.join(q)).unwrap(), b"loser on B");

        let md = std::fs::symlink_metadata(rig.b.tree.join(q)).unwrap();
        let mt = md
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!((mt.as_secs(), mt.subsec_nanos()), (150, 0));

        let log = list_conflicts(&rig.b.state_dir).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].path, "f.txt");
        assert_eq!(log[0].kind, "both_changed");
        assert_eq!(log[0].winner.device, hex(&DEV_A));
        assert_eq!(log[0].loser.device, hex(&DEV_B));
        assert_eq!(log[0].quarantined_as.as_deref(), Some(q.as_str()));

        assert!(result.agreed_manifest_id.is_none());
        assert!(AgreementLedger::new(rig.b.store.store_dir())
            .get(&[7; 16], &DEV_A)
            .unwrap()
            .is_none());
    }

    #[test]
    fn racing_writer_at_landing_time_cannot_be_overwritten() {
        let rig = rig();
        let base = "f.txt.ferry-conflict.b2b2b2b2-19700101-000230";
        let claimed = rig.b.tree.join(base);
        std::fs::write(&claimed, b"racer's loser copy").unwrap();

        let result = converge_on_b(&rig, NOW).unwrap();

        assert_eq!(result.quarantined.len(), 1);
        let q = &result.quarantined[0];
        assert_eq!(
            q.as_str(),
            format!("{base}-2"),
            "loser copy regenerated past the claimed name"
        );
        assert_eq!(
            std::fs::read(&claimed).unwrap(),
            b"racer's loser copy",
            "the racing writer's copy must survive byte-for-byte"
        );
        assert_eq!(std::fs::read(rig.b.tree.join(q)).unwrap(), b"loser on B");
        let log = list_conflicts(&rig.b.state_dir).unwrap();
        assert_eq!(log[0].quarantined_as.as_deref(), Some(q.as_str()));

        let residue: Vec<_> = std::fs::read_dir(&rig.b.tree)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".ferry."))
            .collect();
        assert!(residue.is_empty(), "temp leaked: {residue:?}");
    }

    #[test]
    fn exhausted_landing_fails_loudly_without_clobbering_or_residue() {
        let rig = rig();
        let plan = reconcile(ReconcileInput {
            store: &rig.b.store,
            local: &rig.local,
            remote: &rig.remote,
            base: Some(&rig.base),
        })
        .unwrap();
        let op = &plan.quarantine[0];
        let base = naming::conflict_display_name("f.txt", &op.loser_device, op.loser_mtime_sec);

        for counter in 1..=MAX_LANDING_ATTEMPTS {
            let name = if counter == 1 {
                base.clone()
            } else {
                format!("{base}-{counter}")
            };
            std::fs::write(rig.b.tree.join(name), b"occupied").unwrap();
        }

        let err = write_loser_copy(
            &rig.b.store,
            &rig.b.tree,
            op,
            &rig.b.tree,
            &base,
            Some(b"loser on B" as &[u8]),
        )
        .unwrap_err();

        assert!(
            matches!(err, ConvergenceError::Io { .. }),
            "exhaustion must fail loudly, got {err:?}"
        );

        for counter in 1..=MAX_LANDING_ATTEMPTS {
            let name = if counter == 1 {
                base.clone()
            } else {
                format!("{base}-{counter}")
            };
            assert_eq!(std::fs::read(rig.b.tree.join(&name)).unwrap(), b"occupied");
        }
        let residue: Vec<_> = std::fs::read_dir(&rig.b.tree)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".ferry."))
            .collect();
        assert!(residue.is_empty(), "temp leaked: {residue:?}");
    }

    #[test]
    fn tampered_live_file_surfaces_diverged_before_any_writes() {
        let rig = rig();

        std::fs::write(rig.b.tree.join("f.txt"), b"tampered!!").unwrap();

        let err = converge_on_b(&rig, (1, 0)).unwrap_err();
        match err {
            ConvergenceError::Materialize(MaterializeError::Diverged { paths }) => {
                assert_eq!(paths.len(), 1);
                assert_eq!(join_path(&paths[0].path), "f.txt");
            }
            other => panic!("expected Diverged, got {other:?}"),
        }

        assert_eq!(
            std::fs::read(rig.b.tree.join("f.txt")).unwrap(),
            b"tampered!!",
            "the Expect chain refuses to act on diverged state"
        );
        assert!(list_conflicts(&rig.b.state_dir).unwrap().is_empty());
    }

    #[test]
    fn nfd_disk_spelling_resolves_for_stored_nfc_paths() {
        let mut a = Device::new(6, DEV_A, poly_of(13));
        let mut b = Device::new(7, DEV_B, poly_of(13));

        let nfd = "rapport-anne\u{301}e.md";
        write_file(&a.tree.join(nfd), b"base", false, (100, 0));
        write_file(&b.tree.join(nfd), b"base", false, (100, 0));
        let s0 = a.snapshot();
        let _s0b = b.snapshot();
        transfer_manifest(&a.store, &b.store, &s0.manifest, s0.manifest_id);
        b.parent = s0.manifest_id;

        write_file(&a.tree.join(nfd), b"winner from A", false, (200, 0));
        write_file(&b.tree.join(nfd), b"loser on B", false, (150, 0));
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);

        let mut fetch = PeerFetch {
            from: &a.store,
            to: &b.store,
        };
        let result = ConvergenceEngine::new(&b.store, &b.tree)
            .state_dir(&b.state_dir)
            .at(NOW)
            .fetch_with(&mut fetch)
            .converge(&sb.manifest, &sa.manifest, Some(&s0.manifest))
            .unwrap();

        assert_eq!(result.quarantined.len(), 1);
        assert!(
            result.quarantined[0].starts_with("rapport-ann\u{e9}e.md.ferry-conflict."),
            "NFC-composed quarantine name: {}",
            result.quarantined[0]
        );
        assert_eq!(
            std::fs::read(b.tree.join(nfd)).unwrap(),
            b"winner from A",
            "winner lands on the decomposed spelling the filesystem holds"
        );
        assert_eq!(
            std::fs::read(b.tree.join(&result.quarantined[0])).unwrap(),
            b"loser on B"
        );
    }

    #[test]
    fn identical_trees_converge_to_zero_mutations_and_commit_agreement() {
        let mut a = Device::new(3, DEV_A, poly_of(9));
        let mut b = Device::new(4, DEV_B, poly_of(9));
        write_file(&a.tree.join("x.txt"), b"x", false, (1, 0));
        write_file(&b.tree.join("x.txt"), b"x", false, (1, 0));
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);

        let mut fetch = PeerFetch {
            from: &a.store,
            to: &b.store,
        };
        let result = ConvergenceEngine::new(&b.store, &b.tree)
            .state_dir(&b.state_dir)
            .at(NOW)
            .fetch_with(&mut fetch)
            .converge(&sb.manifest, &sa.manifest, None)
            .unwrap();

        assert!(result.is_noop(), "{result:?}");
        assert!(list_conflicts(&b.state_dir).unwrap().is_empty());

        let agreed = result.agreed_manifest_id.expect("agreement committed");
        assert_eq!(agreed, sa.manifest_id);
        let rec = AgreementLedger::new(b.store.store_dir())
            .get(&[7; 16], &DEV_A)
            .unwrap()
            .expect("ledger record present");
        assert_eq!(rec.manifest_id, sa.manifest_id);
        assert_eq!(rec.peer_device_id, DEV_A);
        assert_eq!((rec.agreed_sec, rec.agreed_nsec), NOW);
    }

    #[test]
    fn resurrection_converges_on_the_deleting_device() {
        let mut a = Device::new(4, DEV_A, poly_of(11));
        let mut b = Device::new(5, DEV_B, poly_of(11));
        write_file(&a.tree.join("f.txt"), b"base", false, (10, 0));
        write_file(&b.tree.join("f.txt"), b"base", false, (10, 0));
        let s0a = a.snapshot();
        let _s0b = b.snapshot();
        transfer_manifest(&a.store, &b.store, &s0a.manifest, s0a.manifest_id);
        b.parent = s0a.manifest_id;

        std::fs::remove_file(b.tree.join("f.txt")).unwrap();
        write_file(&a.tree.join("f.txt"), b"edited on A", false, (20, 0));
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);

        let mut fetch = PeerFetch {
            from: &a.store,
            to: &b.store,
        };
        let result = ConvergenceEngine::new(&b.store, &b.tree)
            .state_dir(&b.state_dir)
            .at(NOW)
            .fetch_with(&mut fetch)
            .converge(&sb.manifest, &sa.manifest, Some(&s0a.manifest))
            .unwrap();

        assert_eq!(
            std::fs::read(b.tree.join("f.txt")).unwrap(),
            b"edited on A",
            "the edit comes back live"
        );
        assert!(result.quarantined.is_empty());
        let log = list_conflicts(&b.state_dir).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, "delete_vs_edit");
        assert_eq!(log[0].loser.mtime_sec, None, "deletion side has no mtime");
        assert_eq!(log[0].quarantined_as, None);
    }

    #[test]
    fn missing_blobs_fail_loudly_before_any_writes() {
        let rig = rig();

        let err = ConvergenceEngine::new(&rig.b.store, &rig.b.tree)
            .state_dir(&rig.b.state_dir)
            .at(NOW)
            .converge(&rig.local, &rig.remote, Some(&rig.base))
            .unwrap_err();
        assert!(
            matches!(err, ConvergenceError::MissingBlobs { count: 1 }),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read(rig.b.tree.join("f.txt")).unwrap(),
            b"loser on B",
            "the tree is untouched"
        );
        let residue: Vec<_> = std::fs::read_dir(&rig.b.tree)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().contains(".ferry-conflict."))
            .collect();
        assert!(residue.is_empty(), "no quarantine appeared: {residue:?}");
        assert!(list_conflicts(&rig.b.state_dir).unwrap().is_empty());
    }

    #[test]
    fn pin_gate_holds_pinned_paths_and_scopes_send() {
        let mut a = Device::new(6, DEV_A, poly_of(17));
        let mut b = Device::new(7, DEV_B, poly_of(17));
        write_file(&a.tree.join("src/a.txt"), b"base src", false, (100, 0));
        write_file(&b.tree.join("src/a.txt"), b"base src", false, (100, 0));
        write_file(&a.tree.join("docs/d.txt"), b"base docs", false, (100, 0));
        write_file(&b.tree.join("docs/d.txt"), b"base docs", false, (100, 0));
        let s0 = a.snapshot();
        let _sb0 = b.snapshot();
        transfer_manifest(&a.store, &b.store, &s0.manifest, s0.manifest_id);
        b.parent = s0.manifest_id;

        write_file(&a.tree.join("src/a.txt"), b"A newer src", false, (300, 0));
        write_file(&b.tree.join("src/a.txt"), b"B older src", false, (200, 0));
        write_file(
            &b.tree.join("docs/d.txt"),
            b"B edited docs",
            false,
            (250, 0),
        );
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);

        let mut fetch = PeerFetch {
            from: &b.store,
            to: &a.store,
        };
        let result = ConvergenceEngine::new(&a.store, &a.tree)
            .state_dir(&a.state_dir)
            .at(NOW)
            .hold(|p| p.first().is_some_and(|c| c == "docs"))
            .fetch_with(&mut fetch)
            .converge(&sa.manifest, &sb.manifest, Some(&s0.manifest))
            .unwrap();

        assert_eq!(result.held.len(), 1, "{result:?}");
        assert_eq!(result.held[0].path, "docs/d.txt");
        assert_eq!(result.held[0].decision, HeldDecision::RemoteApply);
        assert!(!result.held[0].chunks.is_empty());

        assert_eq!(
            std::fs::read(a.tree.join("docs/d.txt")).unwrap(),
            b"base docs"
        );
        assert_eq!(
            std::fs::read(a.tree.join("src/a.txt")).unwrap(),
            b"A newer src"
        );
        assert_eq!(result.quarantined.len(), 1);
        assert_eq!(
            std::fs::read(a.tree.join(&result.quarantined[0])).unwrap(),
            b"B older src"
        );
        assert_eq!(result.conflicts.len(), 1);
        assert_eq!(list_conflicts(&a.state_dir).unwrap()[0].path, "src/a.txt");

        assert!(!result.send.is_empty());

        assert!(result.agreed_manifest_id.is_none());
    }

    #[test]
    fn nested_pin_halves_are_refused_loudly() {
        let (dir, store, local) = store_with_one_file();
        let st = |chunks: Vec<(BlobId, u64)>| EntryState {
            kind: EntryKind::File,
            exec: false,
            mtime_sec: 1,
            mtime_nsec: 0,
            chunks,
            target: None,
        };
        let mut plan = ActionPlan::default();
        plan.materialize.push(crate::reconcile::MaterializeOp {
            path: vec!["src".into()],
            base: None,
            result: None,
        });
        plan.materialize.push(crate::reconcile::MaterializeOp {
            path: vec!["src".into(), "inner.rs".into()],
            base: None,
            result: Some(st(Vec::new())),
        });

        let err = gate_plan(plan, |p| p.last().is_some_and(|c| c == "inner.rs")).unwrap_err();
        let _ = (&store, &local);
        match err {
            ConvergenceError::StructuralSplit { pinned, other } => {
                assert_eq!(pinned, "src/inner.rs");
                assert_eq!(other, "src");
            }
            other => panic!("expected StructuralSplit, got {other:?}"),
        }
        drop(dir);
    }

    #[test]
    fn materialize_ops_fold_into_the_right_applier_buckets() {
        use ferry_store::diff::EntryKind;
        let st = |chunks: Vec<(BlobId, u64)>| EntryState {
            kind: EntryKind::File,
            exec: false,
            mtime_sec: 1,
            mtime_nsec: 2,
            chunks,
            target: None,
        };
        let mut cs = ChangeSet::default();
        fold_into_change_set(None, Some(&st(vec![])), &["a".into()], &mut cs);
        fold_into_change_set(Some(&st(vec![])), None, &["b".into()], &mut cs);
        fold_into_change_set(
            Some(&st(vec![([1u8; 32], 4)])),
            Some(&st(vec![([2u8; 32], 4)])),
            &["c".into()],
            &mut cs,
        );
        fold_into_change_set(
            Some(&st(vec![([1u8; 32], 4)])),
            Some(&st(vec![([1u8; 32], 4)])),
            &["d".into()],
            &mut cs,
        );
        assert_eq!(cs.added.len(), 1);
        assert_eq!(cs.removed.len(), 1);
        assert_eq!(cs.content_modified.len(), 1);
        assert_eq!(cs.metadata_modified.len(), 1);
        assert_eq!(cs.type_changed.len(), 0);
    }

    fn store_with_one_file() -> (tempfile::TempDir, Store, RootManifest) {
        use rand::SeedableRng;
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = Store::create(
            &store_root,
            [7u8; 32],
            Box::new(ferry_store::crypto::PassthroughCipher),
        )
        .unwrap();
        let tree = dir.path().join("tree");
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
            ferry_store::chunker::ValidatedPoly::generate(&mut rand::rngs::StdRng::seed_from_u64(
                3,
            )),
            &tree,
            &idn,
        )
        .unwrap();
        (dir, store, out.manifest)
    }
}
