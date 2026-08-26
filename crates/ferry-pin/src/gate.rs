//! Session pin gate policy for materialization (T-03).
//!
//! Evaluates incoming change sets against the active pin on disk, withholding
//! matching paths from disk mutation and ledgering them to `.ferry/held/<peer>.jsonl`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use ferry_materialize::{MaterializeError, PinGate};
use ferry_store::diff::{join_path, ChangeSet};
use ferry_store::format::hex;
use ferry_store::BlobId;

use crate::{HeldChunk, HeldEntry, HeldLedger, PathMatcher, PinStore};

/// `PinGate` implementation enforcing active session pins during materialization.
#[derive(Clone, Debug)]
pub struct SessionPinGate {
    pub state_dir: PathBuf,
    pub peer_hex: String,
    pub remote_manifest_id_hex: String,
}

impl SessionPinGate {
    pub fn new(
        state_dir: impl Into<PathBuf>,
        peer_hex: impl Into<String>,
        remote_manifest_id_hex: impl Into<String>,
    ) -> Self {
        Self {
            state_dir: state_dir.into(),
            peer_hex: peer_hex.into(),
            remote_manifest_id_hex: remote_manifest_id_hex.into(),
        }
    }
}

impl PinGate for SessionPinGate {
    fn evaluate_changes(
        &self,
        changes: &ChangeSet,
    ) -> Result<(ChangeSet, usize), MaterializeError> {
        // Tolerant fast path: no pin on disk at all is the steady state of
        // most folders; one stat avoids the parse entirely.
        if !PinStore::new(&self.state_dir).path().is_file() {
            return Ok((changes.clone(), 0));
        }
        let Some(rec) = PinStore::new(&self.state_dir)
            .load()
            .map_err(|e| MaterializeError::Pin(e.to_string()))?
        else {
            return Ok((changes.clone(), 0));
        };
        if !rec.holding() {
            return Ok((changes.clone(), 0));
        }
        let matcher = PathMatcher::new(&rec.paths)
            .map_err(|e| MaterializeError::Pin(e.to_string()))?;

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
            return Ok((apply_cs, 0));
        }

        let now = ferry_platform::time::now_unix();
        let entries: Vec<HeldEntry> = held
            .into_iter()
            .map(|(path, kind)| {
                let (decision, chunks) = match kind {
                    Held::RemoteApply(chunks) => ("remote_apply".to_string(), chunks),
                    Held::RemoteDelete => ("remote_delete".to_string(), Vec::new()),
                };
                HeldEntry {
                    held_sec: now.0,
                    held_nsec: now.1,
                    path,
                    device_id: self.peer_hex.clone(),
                    remote_manifest_id: self.remote_manifest_id_hex.clone(),
                    chunks,
                    decision,
                    conflict_winner: None,
                }
            })
            .collect();
        let held_count = entries.len();
        let ledger = HeldLedger::new(&self.state_dir);
        let known: BTreeSet<(String, String)> = ledger
            .load_peer(&self.peer_hex)
            .map_err(|e| MaterializeError::Pin(e.to_string()))?
            .into_iter()
            .map(|e| (e.path, e.remote_manifest_id))
            .collect();
        let fresh: Vec<HeldEntry> = entries
            .into_iter()
            .filter(|e| !known.contains(&(e.path.clone(), e.remote_manifest_id.clone())))
            .collect();
        ledger
            .append(&self.peer_hex, &fresh)
            .map_err(|e| MaterializeError::Pin(e.to_string()))?;

        Ok((apply_cs, held_count))
    }
}

fn chunk(id: &BlobId, len: u64) -> HeldChunk {
    HeldChunk {
        id: hex(id),
        len,
    }
}
