//! The hold seam: what the sync loop consults BEFORE converging, and where
//! held decisions are ledgered after a pinned convergence.

use std::collections::BTreeSet;

use ferry_store::format::hex;

use crate::converge::{HeldDecision, HeldPath, Side};
use crate::held::{HeldChunk, HeldEntry, HeldLedger};
use crate::matcher::PathMatcher;
use crate::pin::PinStore;
use crate::pin_error::PinError;

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
