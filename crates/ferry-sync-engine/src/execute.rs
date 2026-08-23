//! Executes an [`ActionPlan`](crate::plan::ActionPlan) against a live tree.
//!
//! Order of operations, chosen so no step can destroy bytes another step
//! still needs:
//!
//! 1. Resolve every quarantine destination name (collision counters applied
//!    against the live directory).
//! 2. Pre-verify every local loser: read the live file once, check it
//!    region-by-region against the chunk list the local manifest declares.
//!    Any mismatch aborts with `MaterializeError::Diverged` BEFORE anything
//!    on disk has been touched. This is the fast half of the guard; the slow
//!    half is the applier's `Overwrite::Expect`, which re-proves the whole
//!    affected set against the same manifest before mutating.
//! 3. Write quarantine copies (temp + rename, loser's mtime and exec bit;
//!    symlinks are recreated with their target, their link mtime is not
//!    preserved, a documented v1 gap).
//! 4. Fold materialize transitions into one change set and apply it through
//!    ferry-materialize guarded by `Overwrite::Expect { expected: local }`.
//! 5. Append conflict entries to `.ferry/conflicts.jsonl`.
//!
//! If step 4 diverges, quarantine copies from step 3 remain on disk. That is
//! deliberate: they are extra copies, never losses, and the collision rule
//! makes a retry safe.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ferry_materialize::temp::{fresh_entropy, temp_name_for, TempStyle};
use ferry_materialize::{
    Applier, ApplyStats, DivergeReason, Divergence, MaterializeError, Overwrite,
};
use ferry_store::diff::{join_path, ChangeSet, EntryKind, EntryState};
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::store::{Store, StoreError};
use thiserror::Error;

use crate::plan::{ActionPlan, ConflictKind, LoserContent, QuarantineOp};
use crate::report::{append_entries, ConflictEntry, DeviceStamp, LogError};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("materialization failed: {0}")]
    Materialize(#[from] MaterializeError),
    #[error("conflict report failed: {0}")]
    Log(#[from] LogError),
    #[error("io failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn io_at(path: impl Into<PathBuf>, e: std::io::Error) -> EngineError {
    EngineError::Io {
        path: path.into(),
        source: e,
    }
}

/// What one execution actually did.
#[derive(Clone, Debug, Default)]
pub struct ExecuteStats {
    /// Stored relative paths of the loser copies written this run.
    pub quarantined: Vec<String>,
    /// Applier statistics; `mutations() == 0` proves idempotence.
    pub apply: ApplyStats,
    /// Complete report entries for this run (also appended to the JSONL log
    /// when `state_dir` was given).
    pub conflicts: Vec<ConflictEntry>,
}

/// Execute `plan` under `root`.
///
/// `state_dir` receives the conflicts.jsonl appends; pass `None` to skip
/// persistence (entries still come back in [`ExecuteStats`]). `now` stamps
/// report lines only; quarantine names derive from loser mtimes so they
/// stay deterministic across devices.
pub fn execute(
    store: &Store,
    root: &Path,
    plan: &ActionPlan,
    state_dir: Option<&Path>,
    now: (i64, u32),
) -> Result<ExecuteStats, EngineError> {
    // 1 + 2: resolve destinations and pre-verify local losers before any
    // write happens anywhere.
    let mut dest_rel: Vec<String> = Vec::with_capacity(plan.quarantine.len());
    let mut dest_abs: Vec<PathBuf> = Vec::with_capacity(plan.quarantine.len());
    let mut buffers: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
    for (idx, op) in plan.quarantine.iter().enumerate() {
        let name = op.path.last().expect("stored paths are non-empty").clone();
        let parent = &op.path[..op.path.len() - 1];
        let candidate =
            crate::naming::conflict_display_name(&name, &op.loser_device, op.loser_mtime_sec);
        let abs = crate::naming::unique_conflict_dest(root, parent, &candidate)
            .map_err(|e| io_at(root.join(&candidate), e))?;
        if let LoserContent::LiveLocal { expected_chunks } = &op.content {
            let live_abs = abs_under(root, &op.path);
            buffers.insert(idx, read_live_verified(&live_abs, &op.path, expected_chunks)?);
        }
        dest_rel.push(join_path(&rel_under(root, &abs)));
        dest_abs.push(abs);
    }

    // 3: write the loser copies.
    for (idx, op) in plan.quarantine.iter().enumerate() {
        write_loser_copy(store, root, op, &dest_abs[idx], buffers.get(&idx).map(Vec::as_slice))?;
    }

    // 4: fold transitions into one change set and apply under guard.
    let mut cs = ChangeSet::default();
    for op in &plan.materialize {
        fold_into_change_set(op.base.as_ref(), op.result.as_ref(), &op.path, &mut cs);
    }
    let apply = if cs.is_empty() {
        ApplyStats::default()
    } else {
        let expected = plan.guard_expected.as_ref().ok_or(MaterializeError::BadComponent {
            component: "plan missing guard_expected".to_string(),
        })?;
        Applier::new(store, root)
            .overwrite(Overwrite::Expect {
                expected: expected.clone(),
            })
            .apply_change_set(&cs)?
    };

    // 5: report.
    let folder_id = plan
        .guard_expected
        .as_ref()
        .map(|m| hex(&m.folder_id))
        .unwrap_or_default();
    let mut stats = ExecuteStats {
        quarantined: dest_rel.clone(),
        apply,
        conflicts: Vec::new(),
    };
    for c in &plan.conflicts {
        stats.conflicts.push(ConflictEntry {
            ts: crate::timefmt::fmt_rfc3339(now.0),
            folder_id: folder_id.clone(),
            path: join_path(&c.path),
            kind: kind_str(c.kind).to_string(),
            winner: stamp(c.winner_device, Some((c.winner_mtime_sec, c.winner_mtime_nsec))),
            loser: stamp(c.loser_device, c.loser_mtime_sec.map(|s| (s, c.loser_mtime_nsec.unwrap_or(0)))),
            quarantined_as: plan
                .quarantine
                .iter()
                .position(|op| op.path == c.path)
                .and_then(|pos| dest_rel.get(pos).cloned()),
        });
    }
    if let Some(sd) = state_dir {
        append_entries(sd, &stats.conflicts)?;
    }
    Ok(stats)
}

fn kind_str(k: ConflictKind) -> &'static str {
    match k {
        ConflictKind::BothChanged => "both_changed",
        ConflictKind::DeleteVsEdit => "delete_vs_edit",
        ConflictKind::AddVsAdd => "add_vs_add",
    }
}

fn stamp(device: [u8; 32], mtime: Option<(i64, u32)>) -> DeviceStamp {
    DeviceStamp {
        device: hex(&device),
        mtime_sec: mtime.map(|m| m.0),
        mtime_nsec: mtime.map(|m| m.1),
    }
}

fn rel_under(root: &Path, abs: &Path) -> Vec<String> {
    abs.strip_prefix(root)
        .unwrap_or(Path::new(""))
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect()
}

fn abs_under(root: &Path, rel: &[String]) -> PathBuf {
    let mut p = root.to_path_buf();
    for c in rel {
        p.push(c);
    }
    p
}

/// Read the live file and require every declared chunk region to hash to its
/// id, with no trailing bytes beyond the declared content. Any drift
/// surfaces as the applier's own divergence error type so callers see one
/// divergence vocabulary end to end.
fn read_live_verified(
    abs: &Path,
    rel: &[String],
    expected_chunks: &[(BlobId, u64)],
) -> Result<Vec<u8>, EngineError> {
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
        return Err(diverged(rel, DivergeReason::SizeMismatch {
            expected: declared_total,
            found: declared_total + 1,
        }));
    }
    Ok(out)
}

fn diverged(rel: &[String], reason: DivergeReason) -> EngineError {
    EngineError::Materialize(MaterializeError::Diverged {
        paths: vec![Divergence {
            path: rel.to_vec(),
            reason,
        }],
    })
}

/// Write one loser copy atomically (temp + rename inside the destination's
/// parent directory), preserving the loser's exec bit and mtime.
fn write_loser_copy(
    store: &Store,
    root: &Path,
    op: &QuarantineOp,
    abs_dest: &Path,
    live_bytes: Option<&[u8]>,
) -> Result<(), EngineError> {
    if let Some(parent) = abs_dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_at(parent, e))?;
    }
    let display = join_path(&op.path);
    let tmp = abs_dest.parent().expect("dest always has a parent").join(
        temp_name_for(&display, TempStyle::current(), &fresh_entropy()),
    );
    let rename = |tmp: &Path, dest: &Path| -> Result<(), EngineError> {
        std::fs::rename(tmp, dest).map_err(|e| {
            let _ = std::fs::remove_file(tmp);
            io_at(dest, e)
        })
    };

    match &op.content {
        LoserContent::LiveLocalSymlink { expected_target } => {
            // The live link must still point where the manifest says.
            let actual = std::fs::read_link(abs_under(root, &op.path))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            if &actual != expected_target {
                return Err(diverged(&op.path, DivergeReason::TargetMismatch {
                    expected: expected_target.clone(),
                    found: actual,
                }));
            }
            make_symlink(expected_target, &tmp)?;
            rename(&tmp, abs_dest)
        }
        LoserContent::LiveLocal { .. } => {
            let bytes = live_bytes.expect("pre-verified buffer must exist");
            write_bytes_with_meta(&tmp, abs_dest, bytes, op.exec, op.loser_mtime_sec, op.loser_mtime_nsec)
        }
        LoserContent::FromStore {
            kind,
            chunks,
            target,
            ..
        } => {
            if *kind == EntryKind::Symlink {
                let t = target.clone().unwrap_or_default();
                make_symlink(&t, &tmp)?;
                rename(&tmp, abs_dest)
            } else {
                let mut bytes = Vec::with_capacity(chunks.iter().map(|c| c.1 as usize).sum());
                for (id, len) in chunks {
                    let piece = store.get(BlobKind::DataChunk, id).map_err(EngineError::from)?;
                    if piece.len() as u64 != *len {
                        return Err(EngineError::Materialize(MaterializeError::ChunkCorrupt {
                            path: display,
                            index: usize::MAX,
                            expected: format!("len {len}"),
                            found: format!("len {}", piece.len()),
                        }));
                    }
                    bytes.extend_from_slice(&piece);
                }
                write_bytes_with_meta(
                    &tmp,
                    abs_dest,
                    &bytes,
                    op.exec,
                    op.loser_mtime_sec,
                    op.loser_mtime_nsec,
                )
            }
        }
    }
}

fn make_symlink(target: &str, at: &Path) -> Result<(), EngineError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at).map_err(|e| io_at(at, e))
    }
    #[cfg(not(unix))]
    {
        let _ = (target, at);
        Err(EngineError::Io {
            path: PathBuf::new(),
            source: std::io::Error::other("symlinks unsupported"),
        })
    }
}

fn write_bytes_with_meta(
    tmp: &Path,
    dest: &Path,
    bytes: &[u8],
    exec: bool,
    sec: i64,
    nsec: u32,
) -> Result<(), EngineError> {
    use std::io::Write;
    {
        let mut f = std::fs::File::create(tmp).map_err(|e| io_at(tmp, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }))
                .map_err(|e| io_at(tmp, e))?;
        }
        f.write_all(bytes).map_err(|e| io_at(tmp, e))?;
        f.set_times(std::fs::FileTimes::new().set_modified(system_time(sec, nsec)))
            .map_err(|e| io_at(tmp, e))?;
        f.sync_all().map_err(|e| io_at(tmp, e))?;
    }
    std::fs::rename(tmp, dest).map_err(|e| {
        let _ = std::fs::remove_file(tmp);
        io_at(dest, e)
    })
}

/// (sec, nsec) → SystemTime, matching the manifest's pre-1970 convention.
fn system_time(sec: i64, nsec: u32) -> SystemTime {
    let total = sec as i128 * 1_000_000_000 + nsec as i128;
    if total >= 0 {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(total as u64)
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_nanos((-total) as u64)
    }
}

/// Encode one base→result transition into applier buckets. The applier plans
/// against live state itself; the bucket choice only decides removal depth
/// and create-vs-update bookkeeping.
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
