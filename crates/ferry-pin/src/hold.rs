//! The hold seam: what the sync loop consults BEFORE converging, and where
//! held decisions are ledgered after a pinned convergence.
//!
//! With the transactional convergence engine (T-04) the old plan-splitting
//! is gone: the engine itself partitions its internal plan around the pin's
//! globs — apply-now decisions run through the ordinary three-way flow,
//! pinned paths are withheld and returned in [`ConvergenceResult::held`].
//! This module provides the two pin-side pieces of that flow:
//!
//! - [`hold_matcher`]: loads the folder's pin and compiles its globs, so
//!   the sync loop can hand the engine a path gate. A stale or released
//!   pin holds nothing; a corrupt pin is a loud error — never silently
//!   treated as "no pin".
//! - [`record_held`]: maps the engine's held decisions into
//!   [`HeldEntry`] ledger lines under `.ferry/held/<peer>.jsonl`,
//!   deduplicating on (path, remote manifest) so identical rounds append
//!   nothing.

use std::collections::BTreeSet;

use ferry_store::format::hex;
use ferry_sync_engine::{HeldDecision, HeldPath, Side};

use crate::error::PinError;
use crate::held::{HeldChunk, HeldEntry, HeldLedger};
use crate::matcher::PathMatcher;
use crate::pin::PinStore;

/// Load the active pin's scope as a compiled matcher. `None` when no pin
/// exists, is released, or its writer is dead (stale) — stale pins surface
/// elsewhere but never hold.
pub fn hold_matcher(state_dir: &std::path::Path) -> Result<Option<PathMatcher>, PinError> {
    let Some(rec) = PinStore::new(state_dir).load()? else {
        return Ok(None);
    };
    if !rec.holding() {
        return Ok(None);
    }
    Ok(Some(PathMatcher::new(&rec.paths)?))
}

/// Persist one convergence's held decisions to the peer's ledger.
///
/// Returns how many lines were appended. Lines already ledgered for the
/// same (path, remote manifest) pair are skipped: long pins span many
/// poll ticks and identical rounds must append nothing. Call AFTER a
/// successful pinned convergence — the engine guarantees those paths were
/// not touched.
pub fn record_held(
    state_dir: &std::path::Path,
    peer_hex: &str,
    remote_manifest_id_hex: &str,
    held: &[HeldPath],
    now: (i64, u32),
) -> Result<usize, PinError> {
    if held.is_empty() {
        return Ok(0);
    }
    let entries: Vec<HeldEntry> = held
        .iter()
        .map(|h| HeldEntry {
            held_sec: now.0,
            held_nsec: now.1,
            path: h.path.clone(),
            device_id: peer_hex.to_string(),
            remote_manifest_id: remote_manifest_id_hex.to_string(),
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
        .load_peer(peer_hex)?
        .into_iter()
        .map(|e| (e.path, e.remote_manifest_id))
        .collect();
    let fresh: Vec<HeldEntry> = entries
        .into_iter()
        .filter(|e| !known.contains(&(e.path.clone(), e.remote_manifest_id.clone())))
        .collect();
    let count = fresh.len();
    ledger.append(peer_hex, &fresh)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pin::PinRecord;
    use crate::PIN_FORMAT_VERSION;
    use std::collections::BTreeMap;

    fn active_record(paths: &[&str]) -> PinRecord {
        PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: "aa".repeat(32),
            pid: std::process::id(),
            started_sec: 1,
            started_nsec: 0,
            expires_sec: None,
            paths: paths.iter().map(std::string::ToString::to_string).collect(),
            released: false,
            base_agreements: BTreeMap::new(),
            proc_start_token: None,
        }
    }

    #[test]
    fn matcher_compiles_when_holding_and_none_otherwise() {
        let dir = tempfile::tempdir().unwrap();
        let pin = PinStore::new(dir.path());

        assert!(hold_matcher(dir.path()).unwrap().is_none());

        pin.start(&active_record(&["src/**"])).unwrap();
        let m = hold_matcher(dir.path()).unwrap().expect("active pin");
        assert!(m.matches(&["src".into(), "a.rs".into()]));
        assert!(!m.matches(&["docs".into(), "x".into()]));

        // Stale (dead writer) never holds: a real process, killed, its pid
        // stranded in the record.
        pin.mark_released().unwrap();
        let mut child = ferry_platform::spawn_sleeper(30).expect("spawn sleeper");
        let dead = {
            child.kill().expect("kill -9 equivalent");
            child.wait().expect("reap");
            child.id()
        };
        pin.start(&PinRecord {
            pid: dead,
            ..active_record(&["src/**"])
        })
        .unwrap();
        assert!(hold_matcher(dir.path()).unwrap().is_none());

        // Released never holds.
        pin.start(&active_record(&["src/**"])).unwrap();
        pin.mark_released().unwrap();
        assert!(hold_matcher(dir.path()).unwrap().is_none());
    }

    #[test]
    fn record_held_maps_decisions_dedups_and_counts() {
        let dir = tempfile::tempdir().unwrap();
        let held = vec![
            HeldPath {
                path: "docs/d.txt".into(),
                decision: HeldDecision::RemoteApply,
                chunks: vec![([9u8; 32], 4)],
            },
            HeldPath {
                path: "gone.txt".into(),
                decision: HeldDecision::RemoteDelete,
                chunks: Vec::new(),
            },
            HeldPath {
                path: "src/a.txt".into(),
                decision: HeldDecision::Conflict {
                    winner: Some(Side::Local),
                },
                chunks: vec![([7u8; 32], 2)],
            },
        ];

        let n = record_held(
            dir.path(),
            &"bb".repeat(32),
            &"cc".repeat(32),
            &held,
            (5, 6),
        )
        .unwrap();
        assert_eq!(n, 3);

        let ledger = HeldLedger::new(dir.path());
        let entries = ledger.load_peer(&"bb".repeat(32)).unwrap();
        assert_eq!(entries.len(), 3);
        let docs = entries.iter().find(|e| e.path == "docs/d.txt").unwrap();
        assert_eq!(docs.decision, "remote_apply");
        assert_eq!(docs.chunks.len(), 1);
        assert_eq!(docs.chunks[0].id, hex(&[9u8; 32]));
        assert_eq!((docs.held_sec, docs.held_nsec), (5, 6));
        let gone = entries.iter().find(|e| e.path == "gone.txt").unwrap();
        assert_eq!(gone.decision, "remote_delete");
        assert!(gone.chunks.is_empty());
        let con = entries.iter().find(|e| e.path == "src/a.txt").unwrap();
        assert_eq!(con.decision, "conflict");
        assert_eq!(con.conflict_winner.as_deref(), Some("local"));

        // Identical round: nothing appended.
        let n2 = record_held(
            dir.path(),
            &"bb".repeat(32),
            &"cc".repeat(32),
            &held,
            (9, 9),
        )
        .unwrap();
        assert_eq!(n2, 0);
        assert_eq!(ledger.load_peer(&"bb".repeat(32)).unwrap().len(), 3);

        // Empty held set is a no-op.
        let n3 = record_held(dir.path(), &"bb".repeat(32), &"cc".repeat(32), &[], (1, 1)).unwrap();
        assert_eq!(n3, 0);
    }
}
