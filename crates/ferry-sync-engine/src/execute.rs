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
//! 3. Write quarantine copies (temp + CREATE-EXCLUSIVE landing, loser's
//!    mtime and exec bit; symlinks are recreated with their target, their
//!    link mtime is not preserved, a documented v1 gap). Landing is
//!    exclusive so a cross-process executor that resolved the same
//!    conflict name free can never have its copy overwritten (ADR-0004:
//!    loser bytes are never destroyed); on collision the name is
//!    regenerated and the attempt retried, bounded.
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

/// Bounded create-exclusive landing attempts per loser copy: `NAME`, then
/// `NAME-2`, `NAME-3`, ... Exhausting the bound means the directory is
/// pathologically contended; failing loudly beats clobbering a racing
/// writer's copy or leaking temp residue (ADR-0004).
const MAX_LANDING_ATTEMPTS: u32 = 128;

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
            // Stored names are NFC; the live file may hold NFD bytes on
            // byte-preserving hosts. Resolve through the applier's own
            // folding so pre-verify reads what a guarded apply would touch.
            let live_abs = ferry_materialize::resolve_live(root, &op.path);
            buffers.insert(
                idx,
                read_live_verified(&live_abs, &op.path, expected_chunks)?,
            );
        }
        dest_rel.push(join_path(&rel_under(root, &abs)));
        dest_abs.push(abs);
    }

    // 3: write the loser copies. Landing is create-exclusive: if a racing
    // executor claimed the resolved name in the meantime, the copy lands
    // under the next counter name and the report reflects where it really
    // went.
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
            store,
            root,
            op,
            &dest_dir,
            &candidate_base,
            buffers.get(&idx).map(Vec::as_slice),
        )?;
        dest_rel[idx] = join_path(&rel_under(root, &landed));
        dest_abs[idx] = landed;
    }

    // 4: fold transitions into one change set and apply under guard.
    let mut cs = ChangeSet::default();
    for op in &plan.materialize {
        fold_into_change_set(op.base.as_ref(), op.result.as_ref(), &op.path, &mut cs);
    }
    let apply = if cs.is_empty() {
        ApplyStats::default()
    } else {
        let expected = plan
            .guard_expected
            .as_ref()
            .ok_or(MaterializeError::BadComponent {
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
            ts: ferry_platform::time::fmt_rfc3339(now.0),
            folder_id: folder_id.clone(),
            path: join_path(&c.path),
            kind: kind_str(c.kind).to_string(),
            winner: stamp(
                c.winner_device,
                Some((c.winner_mtime_sec, c.winner_mtime_nsec)),
            ),
            loser: stamp(
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

fn diverged(rel: &[String], reason: DivergeReason) -> EngineError {
    EngineError::Materialize(MaterializeError::Diverged {
        paths: vec![Divergence {
            path: rel.to_vec(),
            reason,
        }],
    })
}

/// Write one loser copy with create-exclusive landing, preserving the
/// loser's exec bit and mtime.
///
/// The temp sibling is built first, then placed at `candidate_base` — or,
/// if a cross-process executor claimed that name between resolution and
/// landing, at the next counter name (`candidate_base-2`, `-3`, ...),
/// bounded by [`MAX_LANDING_ATTEMPTS`]. Returns the absolute path the copy
/// actually landed at. Every failure path removes the temp it created; on
/// bounded-exhaustion the error is loud rather than an overwrite.
fn write_loser_copy(
    store: &Store,
    root: &Path,
    op: &QuarantineOp,
    dest_dir: &Path,
    candidate_base: &str,
    live_bytes: Option<&[u8]>,
) -> Result<PathBuf, EngineError> {
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
                // Lost the race for this name; regenerate and retry.
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

/// Materialize one loser's bytes (or symlink) at `tmp`. No landing happens
/// here; the caller places `tmp` exclusively.
fn build_tmp(
    store: &Store,
    root: &Path,
    op: &QuarantineOp,
    tmp: &Path,
    live_bytes: Option<&[u8]>,
) -> Result<(), EngineError> {
    match &op.content {
        LoserContent::LiveLocalSymlink { expected_target } => {
            // The live link must still point where the manifest says.
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
                        .map_err(EngineError::from)?;
                    if piece.len() as u64 != *len {
                        return Err(EngineError::Materialize(MaterializeError::ChunkCorrupt {
                            path: join_path(&op.path),
                            index: usize::MAX,
                            expected: format!("len {len}"),
                            found: format!("len {}", piece.len()),
                        }));
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

/// Place `tmp` at `dest` only when nothing occupies `dest` yet.
///
/// Unix: `link()` is atomic create-unless-exists and never follows
/// symlinks, so loser symlinks land as links, not as their targets. Other
/// platforms (Windows included): reserve the name with an
/// `O_CREAT|O_EXCL` probe (`CREATE_NEW`) immediately before the rename,
/// then rename over our own empty reservation.
fn rename_exclusive(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::hard_link(tmp, dest)?;
        // Landing succeeded; dropping our temp name is best-effort (its
        // bytes now also exist under dest, so residue is never data loss).
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
                // Remove our own reservation so the failure path leaves no
                // zero-byte impostor at the conflict name.
                let _ = std::fs::remove_file(dest);
                Err(e)
            }
        }
    }
}

fn make_symlink(target: &str, at: &Path) -> Result<(), EngineError> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, at).map_err(|e| io_at(at, e))
    }
    #[cfg(windows)]
    {
        // std has no generic `symlink`: pick the file vs dir flavor from
        // the target's own metadata (targets are relative, in-tree paths
        // per ferry-platform's links policy). Needs developer mode/admin
        // on Windows; failure surfaces loudly.
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
        Err(EngineError::Io {
            path: PathBuf::new(),
            source: std::io::Error::other("symlinks unsupported"),
        })
    }
}

/// Write the loser's bytes to `tmp` with its exec bit and mtime restored.
/// Landing at the final name happens separately, exclusively.
fn write_bytes_with_meta(
    tmp: &Path,
    bytes: &[u8],
    exec: bool,
    sec: i64,
    nsec: u32,
) -> Result<(), EngineError> {
    use std::io::Write;
    // The exec bit is a unix permission concept; on other platforms it is
    // carried in manifests but not enforced by the filesystem (same
    // convention as apply.rs's non-unix arm).
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
        f.set_times(std::fs::FileTimes::new().set_modified(system_time(sec, nsec)))
            .map_err(|e| io_at(tmp, e))?;
        f.sync_all().map_err(|e| io_at(tmp, e))?;
    }
    Ok(())
}

/// (sec, nsec) → `SystemTime`, matching the manifest's pre-1970 convention.
fn system_time(sec: i64, nsec: u32) -> SystemTime {
    let total = i128::from(sec) * 1_000_000_000 + i128::from(nsec);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{MaterializeOp, Side};
    use crate::reconcile::{reconcile, ReconcileInput};
    use crate::report::list_conflicts;
    use crate::testutil::*;

    const DEV_A: [u8; 32] = [0xA1; 32];
    const DEV_B: [u8; 32] = [0xB2; 32];

    /// Both devices start from an identical base file; A writes newer
    /// content so A wins everywhere; manifests are exchanged and the plan
    /// is computed ON B (whose live copy must be saved then overwritten).
    struct Rig {
        b: Device,
        plan_for_b: ActionPlan,
    }

    fn rig() -> Rig {
        let mut a = Device::new(1, DEV_A, poly_of(5));
        let mut b = Device::new(2, DEV_B, poly_of(5));
        write_file(&a.tree.join("f.txt"), b"base", false, (100, 0));
        write_file(&b.tree.join("f.txt"), b"base", false, (100, 0));
        let s0a = a.snapshot();
        let _s0b = b.snapshot();

        write_file(&a.tree.join("f.txt"), b"winner from A", false, (200, 0));
        write_file(&b.tree.join("f.txt"), b"loser on B", false, (150, 0));
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);

        let mut plan_for_b = reconcile(ReconcileInput {
            store: &b.store,
            local: &sb.manifest,
            remote: &sa.manifest,
            base: Some(&s0a.manifest),
        })
        .unwrap();
        // Simulate fetching the winner blobs before executing.
        for (id, _) in &plan_for_b.fetch {
            transfer(
                &a.store,
                &b.store,
                &[(ferry_store::format::BlobKind::DataChunk, *id)],
            );
        }
        plan_for_b.guard_expected = Some(sb.manifest.clone());
        Rig { b, plan_for_b }
    }

    #[test]
    fn local_loser_saved_then_overwritten_by_winner() {
        let rig = rig();
        let stats = execute(
            &rig.b.store,
            &rig.b.tree,
            &rig.plan_for_b,
            Some(&rig.b.state_dir),
            (1_787_574_896, 0),
        )
        .unwrap();

        // Winner live, loser quarantined under B's own device tag.
        assert_eq!(
            std::fs::read(rig.b.tree.join("f.txt")).unwrap(),
            b"winner from A"
        );
        assert_eq!(stats.quarantined.len(), 1);
        let q = &stats.quarantined[0];
        assert_eq!(
            q, "f.txt.ferry-conflict.b2b2b2b2-19700101-000230",
            "name carries LOSER device short id + loser mtime UTC"
        );
        assert_eq!(std::fs::read(rig.b.tree.join(q)).unwrap(), b"loser on B");
        // The copy keeps the loser's mtime so devices converge exactly.
        let md = std::fs::symlink_metadata(rig.b.tree.join(q)).unwrap();
        let mt = md
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!((mt.as_secs(), mt.subsec_nanos()), (150, 0));

        // Report entry persisted and complete.
        let log = list_conflicts(&rig.b.state_dir).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].path, "f.txt");
        assert_eq!(log[0].kind, "both_changed");
        assert_eq!(log[0].winner.device, hex(&DEV_A));
        assert_eq!(log[0].loser.device, hex(&DEV_B));
        assert_eq!(log[0].quarantined_as.as_deref(), Some(q.as_str()));
    }

    #[test]
    fn racing_writer_at_landing_time_cannot_be_overwritten() {
        // T-08: another executor (CLI sync while daemon runs) resolves the
        // same conflict name free, then claims it AFTER our resolution but
        // BEFORE our landing. The exclusive landing must regenerate the
        // name and never overwrite the racer's bytes.
        let rig = rig();
        let op = &rig.plan_for_b.quarantine[0];
        let base =
            crate::naming::conflict_display_name("f.txt", &op.loser_device, op.loser_mtime_sec);
        let resolved = crate::naming::unique_conflict_dest(&rig.b.tree, &[], &base).unwrap();

        // The racer wins the name in the resolution→landing window.
        std::fs::write(&resolved, b"racer's loser copy").unwrap();

        let landed = write_loser_copy(
            &rig.b.store,
            &rig.b.tree,
            op,
            resolved.parent().unwrap(),
            resolved.file_name().unwrap().to_str().unwrap(),
            Some(b"loser on B" as &[u8]),
        )
        .unwrap();

        assert!(
            landed
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .ends_with("-2"),
            "loser copy regenerated past the claimed name: {landed:?}"
        );
        assert_eq!(
            std::fs::read(&resolved).unwrap(),
            b"racer's loser copy",
            "the racing writer's copy must survive byte-for-byte"
        );
        assert_eq!(std::fs::read(&landed).unwrap(), b"loser on B");
        // No temp residue from the collision retry.
        let residue: Vec<_> = std::fs::read_dir(&rig.b.tree)
            .unwrap()
            .map(|e| e.unwrap())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".ferry."))
            .collect();
        assert!(residue.is_empty(), "temp leaked: {residue:?}");
    }

    #[test]
    fn execute_reports_where_a_contended_landing_actually_went() {
        // Same race end-to-end through execute(): pre-create the first
        // choice so the report's quarantined_as follows the regenerated
        // name, and both copies keep their own bytes.
        let rig = rig();
        let op = &rig.plan_for_b.quarantine[0];
        let base =
            crate::naming::conflict_display_name("f.txt", &op.loser_device, op.loser_mtime_sec);
        let claimed = rig.b.tree.join(&base);
        std::fs::write(&claimed, b"racer's loser copy").unwrap();

        let stats = execute(
            &rig.b.store,
            &rig.b.tree,
            &rig.plan_for_b,
            Some(&rig.b.state_dir),
            (1_787_574_896, 0),
        )
        .unwrap();

        assert_eq!(stats.quarantined.len(), 1);
        let q = &stats.quarantined[0];
        assert_eq!(
            q.as_str(),
            format!("{base}-2"),
            "report uses the real landing name"
        );
        assert_eq!(std::fs::read(&claimed).unwrap(), b"racer's loser copy");
        assert_eq!(std::fs::read(rig.b.tree.join(q)).unwrap(), b"loser on B");
        let log = list_conflicts(&rig.b.state_dir).unwrap();
        assert_eq!(log[0].quarantined_as.as_deref(), Some(q.as_str()));
    }

    #[test]
    fn exhausted_landing_fails_loudly_without_clobbering_or_residue() {
        let rig = rig();
        let op = &rig.plan_for_b.quarantine[0];
        let base =
            crate::naming::conflict_display_name("f.txt", &op.loser_device, op.loser_mtime_sec);
        // Occupy every candidate name the bounded retry could reach.
        for counter in 1..=super::MAX_LANDING_ATTEMPTS {
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
            matches!(err, EngineError::Io { .. }),
            "exhaustion must fail loudly, got {err:?}"
        );
        // Every occupied name keeps its owner's bytes; no temp remains.
        for counter in 1..=super::MAX_LANDING_ATTEMPTS {
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
        // Tamper AFTER the snapshot the plan was computed from.
        std::fs::write(rig.b.tree.join("f.txt"), b"tampered!!").unwrap();

        let err = execute(
            &rig.b.store,
            &rig.b.tree,
            &rig.plan_for_b,
            Some(&rig.b.state_dir),
            (1, 0),
        )
        .unwrap_err();
        match err {
            EngineError::Materialize(MaterializeError::Diverged { paths }) => {
                assert_eq!(paths.len(), 1);
                assert_eq!(join_path(&paths[0].path), "f.txt");
            }
            other => panic!("expected Diverged, got {other:?}"),
        }
        // Nothing was clobbered and no quarantine appeared.
        assert_eq!(
            std::fs::read(rig.b.tree.join("f.txt")).unwrap(),
            b"tampered!!",
            "the Expect chain refuses to act on diverged state"
        );
        assert!(list_conflicts(&rig.b.state_dir).unwrap().is_empty());
    }

    #[test]
    fn nfd_disk_spelling_resolves_for_stored_nfc_paths() {
        // T-012 regression (failed only on byte-preserving Linux CI): scan
        // stores NFC names while the filesystem keeps the decomposed bytes
        // on disk. Quarantine pre-verify must read the loser's live file
        // through the same NFC folding the applier applies, not a bare
        // join, or execute reports a phantom ExpectedPresent before any
        // guarded apply runs.
        let mut a = Device::new(6, DEV_A, poly_of(13));
        let mut b = Device::new(7, DEV_B, poly_of(13));
        // Decomposed spelling ON DISK; every manifest will carry NFC.
        let nfd = "rapport-anne\u{301}e.md";
        write_file(&a.tree.join(nfd), b"base", false, (100, 0));
        write_file(&b.tree.join(nfd), b"base", false, (100, 0));
        let s0 = a.snapshot();
        let _s0b = b.snapshot();

        write_file(&a.tree.join(nfd), b"winner from A", false, (200, 0));
        write_file(&b.tree.join(nfd), b"loser on B", false, (150, 0));
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);

        let mut plan = reconcile(ReconcileInput {
            store: &b.store,
            local: &sb.manifest,
            remote: &sa.manifest,
            base: Some(&s0.manifest),
        })
        .unwrap();
        for (id, _) in &plan.fetch {
            transfer(
                &a.store,
                &b.store,
                &[(ferry_store::format::BlobKind::DataChunk, *id)],
            );
        }
        plan.guard_expected = Some(sb.manifest.clone());

        let stats = execute(
            &b.store,
            &b.tree,
            &plan,
            Some(&b.state_dir),
            (1_787_574_896, 0),
        )
        .unwrap();

        assert_eq!(stats.quarantined.len(), 1);
        assert!(
            stats.quarantined[0].starts_with("rapport-ann\u{e9}e.md.ferry-conflict."),
            "NFC-composed quarantine name: {}",
            stats.quarantined[0]
        );
        assert_eq!(
            std::fs::read(b.tree.join(nfd)).unwrap(),
            b"winner from A",
            "winner lands on the decomposed spelling the filesystem holds"
        );
        assert_eq!(
            std::fs::read(b.tree.join(&stats.quarantined[0])).unwrap(),
            b"loser on B"
        );
    }

    #[test]
    fn empty_plan_is_zero_mutations_and_no_report_lines() {
        let mut a = Device::new(3, DEV_A, poly_of(9));
        write_file(&a.tree.join("x.txt"), b"x", false, (1, 0));
        let _ = a.snapshot();
        let plan = ActionPlan::default();
        let stats = execute(&a.store, &a.tree, &plan, Some(&a.state_dir), (5, 0)).unwrap();
        assert_eq!(stats.apply.mutations(), 0);
        assert!(stats.conflicts.is_empty());
        assert!(list_conflicts(&a.state_dir).unwrap().is_empty());
    }

    #[test]
    fn resurrection_executes_on_the_deleting_device() {
        let mut a = Device::new(4, DEV_A, poly_of(11));
        let mut b = Device::new(5, DEV_B, poly_of(11));
        write_file(&a.tree.join("f.txt"), b"base", false, (10, 0));
        write_file(&b.tree.join("f.txt"), b"base", false, (10, 0));
        let s0a = a.snapshot();
        let _s0b = b.snapshot();

        // B deletes while A edits; compute A's view? No — this rig runs on B
        // so B resurrects A's edit.
        std::fs::remove_file(b.tree.join("f.txt")).unwrap();
        write_file(&a.tree.join("f.txt"), b"edited on A", false, (20, 0));
        let sa = a.snapshot();
        let sb = b.snapshot();
        transfer_manifest(&a.store, &b.store, &sa.manifest, sa.manifest_id);
        transfer_manifest(&b.store, &a.store, &sb.manifest, sb.manifest_id);

        let plan = reconcile(ReconcileInput {
            store: &b.store,
            local: &sb.manifest,
            remote: &sa.manifest,
            base: Some(&s0a.manifest),
        })
        .unwrap();
        assert_eq!(plan.conflicts[0].kind, ConflictKind::DeleteVsEdit);
        assert_eq!(plan.conflicts[0].winner, Side::Remote);
        for (id, _) in &plan.fetch {
            transfer(
                &a.store,
                &b.store,
                &[(ferry_store::format::BlobKind::DataChunk, *id)],
            );
        }

        let stats = execute(&b.store, &b.tree, &plan, Some(&b.state_dir), (9, 0)).unwrap();
        assert_eq!(
            std::fs::read(b.tree.join("f.txt")).unwrap(),
            b"edited on A",
            "the edit comes back live"
        );
        assert!(stats.quarantined.is_empty());
        let log = list_conflicts(&b.state_dir).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].kind, "delete_vs_edit");
        assert_eq!(log[0].loser.mtime_sec, None, "deletion side has no mtime");
        assert_eq!(log[0].quarantined_as, None);
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
        let _ = MaterializeOp {
            path: vec![],
            base: None,
            result: None,
        };
    }
}
