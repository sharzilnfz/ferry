//! Unified high-level manager for session pinning, held ledgers, and release.
//!
//! [`PinManager`] encapsulates [`PinStore`], [`HeldLedger`], [`PathMatcher`],
//! and release planning behind a cohesive interface. Daemon status, CLI commands,
//! and sync engines interact with pin state through `PinManager` rather than
//! manually coordinating procedural primitives.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ferry_store::manifest::RootManifest;
use ferry_store::store::Store;
use serde::{Deserialize, Serialize};

use crate::error::PinError;
use crate::held::{distinct_paths, HeldEntry, HeldLedger};
use crate::matcher::PathMatcher;
use crate::pin::{PinRecord, PinStore, PIN_FORMAT_VERSION};
use crate::release::ReleasePeerPlan;

/// Unified summary of active pin status and per-peer held sets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldSummary {
    /// Active pin status: `"none"`, `"active"`, `"stale"`, or `"released"`.
    pub state: String,
    /// True while the pin is unreleased and its writer process is alive.
    pub holding: bool,
    /// Pinned path patterns (e.g. `["src/**"]` or `["*"]`).
    pub paths: Vec<String>,
    /// Device id (64 lowercase hex) of the pinning device, if recorded.
    pub device_id: Option<String>,
    /// Process id of the declared writer, if recorded.
    pub pid: Option<u32>,
    /// Start timestamp (unix seconds), if recorded.
    pub started_sec: Option<i64>,
    /// Start timestamp (unix nanoseconds), if recorded.
    pub started_nsec: Option<u32>,
    /// Total distinct held path count across all peers.
    pub total_held_paths: usize,
    /// Deduplicated held paths per peer hex (`peer_hex` -> sorted distinct paths).
    pub held_by_peer: BTreeMap<String, Vec<String>>,
}

impl HeldSummary {
    /// Create an empty summary representing no pin and no held changes.
    pub fn none() -> Self {
        Self {
            state: "none".to_string(),
            holding: false,
            paths: Vec::new(),
            device_id: None,
            pid: None,
            started_sec: None,
            started_nsec: None,
            total_held_paths: 0,
            held_by_peer: BTreeMap::new(),
        }
    }
}

/// Detailed outcome of releasing one peer's held entries.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleasePeerResult {
    pub device_id: String,
    pub remote_manifest_id: String,
    pub held_entries: usize,
    pub held_paths: Vec<String>,
    pub ops_applied: usize,
    pub quarantined: usize,
    pub conflicts_recorded: usize,
}

/// Outcome of a transactional release across all held peers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReleaseSummary {
    pub peers: Vec<ReleasePeerResult>,
    pub total_quarantined: usize,
    pub total_conflicts: usize,
    pub total_ops: usize,
    pub pin_ended: bool,
}

/// Cohesive manager coordinating pin lifecycle, held-entry ledgers,
/// glob path validation, and release reconciliation.
#[derive(Clone, Debug)]
pub struct PinManager {
    state_dir: PathBuf,
    store: PinStore,
    ledger: HeldLedger,
}

impl PinManager {
    /// `state_dir` is the folder's `.ferry` directory.
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        let state_dir = state_dir.into();
        let store = PinStore::new(&state_dir);
        let ledger = HeldLedger::new(&state_dir);
        Self {
            state_dir,
            store,
            ledger,
        }
    }

    /// Path to the folder's `.ferry` state directory.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Access the underlying [`PinStore`].
    pub fn store(&self) -> &PinStore {
        &self.store
    }

    /// Access the underlying [`HeldLedger`].
    pub fn ledger(&self) -> &HeldLedger {
        &self.ledger
    }

    /// Load the current [`PinRecord`], if any.
    pub fn record(&self) -> Result<Option<PinRecord>, PinError> {
        self.store.load()
    }

    /// True while a valid pin is active (unreleased and writer is alive).
    pub fn is_holding(&self) -> Result<bool, PinError> {
        Ok(self.record()?.as_ref().is_some_and(PinRecord::holding))
    }

    /// Compute a unified summary of active pin state and all peer held ledgers.
    pub fn summary(&self) -> Result<HeldSummary, PinError> {
        let record = self.store.load()?;
        let (state, holding, paths, device_id, pid, started_sec, started_nsec) = match &record {
            None => (
                "none".to_string(),
                false,
                Vec::new(),
                None,
                None,
                None,
                None,
            ),
            Some(rec) => {
                let s = if rec.released {
                    "released"
                } else if rec.holding() {
                    "active"
                } else {
                    "stale"
                };
                (
                    s.to_string(),
                    rec.holding(),
                    rec.paths.clone(),
                    Some(rec.device_id.clone()),
                    Some(rec.pid),
                    Some(rec.started_sec),
                    Some(rec.started_nsec),
                )
            }
        };

        let mut held_by_peer = BTreeMap::new();
        let mut total_held_paths = 0usize;
        for peer in self.ledger.peers()? {
            let entries = self.ledger.load_peer(&peer)?;
            if !entries.is_empty() {
                let distinct = distinct_paths(&entries);
                total_held_paths += distinct.len();
                held_by_peer.insert(peer, distinct);
            }
        }

        Ok(HeldSummary {
            state,
            holding,
            paths,
            device_id,
            pid,
            started_sec,
            started_nsec,
            total_held_paths,
            held_by_peer,
        })
    }

    /// Start a session pin for the given writer and path patterns with optional duration in seconds.
    ///
    /// Validates path patterns before writing; empty paths default to `["*"]`.
    pub fn start_session_with_duration(
        &self,
        paths: Vec<String>,
        pid: u32,
        identity: &str,
        base_agreements: BTreeMap<String, String>,
        duration_secs: Option<u64>,
    ) -> Result<PinRecord, PinError> {
        let scope = if paths.is_empty() {
            vec!["*".to_string()]
        } else {
            paths
        };

        // Validate glob patterns before writing state
        PathMatcher::new(&scope)?;

        let (sec, nsec) = ferry_platform::now_unix();
        let expires_sec = duration_secs.map(|d| sec + d as i64);
        let record = PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: identity.to_string(),
            pid,
            started_sec: sec,
            started_nsec: nsec,
            expires_sec,
            paths: scope,
            released: false,
            base_agreements,
            proc_start_token: None,
        };

        self.store.start(&record)?;
        Ok(record)
    }

    /// Start a session pin for the given writer and path patterns.
    ///
    /// Validates path patterns before writing; empty paths default to `["*"]`.
    pub fn start_session(
        &self,
        paths: Vec<String>,
        pid: u32,
        identity: &str,
        base_agreements: BTreeMap<String, String>,
    ) -> Result<PinRecord, PinError> {
        self.start_session_with_duration(paths, pid, identity, base_agreements, None)
    }

    /// End the active session pin by marking it released.
    ///
    /// Returns `true` if a pin record existed.
    pub fn stop_session(&self) -> Result<bool, PinError> {
        self.store.mark_released()
    }

    /// Release one peer's held set through the transactional convergence
    /// engine, reconciling the peer's freshest held manifest against the
    /// tree as it is now. The caller clears the peer's ledger
    /// ([`PinManager::clear_peer`]) after `Ok` — a failed release leaves
    /// everything retryable.
    pub fn release_peer(
        &self,
        peer_hex: &str,
        store: &Store,
        root: &Path,
        local_manifest: &RootManifest,
        agreed_base: Option<&RootManifest>,
        now: (i64, u32),
    ) -> Result<ReleasePeerPlan, PinError> {
        let base_owned = match agreed_base {
            Some(_) => None,
            None => {
                let rec = self.store.load()?;
                rec.as_ref().and_then(|r| {
                    r.base_agreements.get(peer_hex).and_then(|b_hex| {
                        crate::release::load_manifest(
                            store,
                            b_hex,
                            peer_hex,
                            "captured as last-agreed at pin start".to_string(),
                        )
                        .ok()
                    })
                })
            }
        };
        crate::release::release_peer(
            store,
            root,
            &self.state_dir,
            local_manifest,
            peer_hex,
            agreed_base.or(base_owned.as_ref()),
            now,
        )
    }

    /// Clear one peer's held ledger after successful release.
    pub fn clear_peer(&self, peer_hex: &str) -> Result<bool, PinError> {
        self.ledger.clear_peer(peer_hex)
    }

    /// Transactionally release all held peer changes through the convergence engine.
    ///
    /// Reconciles held entries per peer, updates the tree, clears each peer's
    /// ledger atomically upon successful convergence, and ends the active pin session.
    ///
    /// If convergence fails for any peer, the error is returned immediately and
    /// remaining held entries remain intact on disk for retry.
    pub fn release(
        &self,
        store: &Store,
        root: &Path,
        local_manifest: &RootManifest,
        now: (i64, u32),
    ) -> Result<ReleaseSummary, PinError> {
        let mut peer_results = Vec::new();
        let mut total_quarantined = 0;
        let mut total_conflicts = 0;
        let mut total_ops = 0;

        let peers = self.held_peers()?;
        for peer_hex in peers {
            let plan = self.release_peer(&peer_hex, store, root, local_manifest, None, now)?;
            if plan.held_entries == 0 {
                continue;
            }
            self.clear_peer(&peer_hex)?;

            let quarantined = plan.result.quarantined.len();
            let conflicts_recorded = plan.result.conflicts.len();
            let ops_applied = plan.result.apply.mutations();

            total_quarantined += quarantined;
            total_conflicts += conflicts_recorded;
            total_ops += ops_applied;

            peer_results.push(ReleasePeerResult {
                device_id: plan.device_id,
                remote_manifest_id: plan.remote_manifest_id,
                held_entries: plan.held_entries,
                held_paths: plan.held_paths,
                ops_applied,
                quarantined,
                conflicts_recorded,
            });
        }

        let pin_ended = self.stop_session()?;

        Ok(ReleaseSummary {
            peers: peer_results,
            total_quarantined,
            total_conflicts,
            total_ops,
            pin_ended,
        })
    }

    /// Reconcile and clear one peer's held ledger atomically.
    pub fn release_peer_transactional(
        &self,
        peer_hex: &str,
        store: &Store,
        root: &Path,
        local_manifest: &RootManifest,
        agreed_base: Option<&RootManifest>,
        now: (i64, u32),
    ) -> Result<ReleasePeerPlan, PinError> {
        let plan = self.release_peer(peer_hex, store, root, local_manifest, agreed_base, now)?;
        if plan.held_entries > 0 {
            self.clear_peer(peer_hex)?;
        }
        Ok(plan)
    }

    /// Append a batch of held entries for one peer.
    pub fn append_held(&self, peer_hex: &str, entries: &[HeldEntry]) -> Result<(), PinError> {
        self.ledger.append(peer_hex, entries)
    }

    /// Load held entries for one peer.
    pub fn load_held_peer(&self, peer_hex: &str) -> Result<Vec<HeldEntry>, PinError> {
        self.ledger.load_peer(peer_hex)
    }

    /// List all peers with held ledgers.
    pub fn held_peers(&self) -> Result<Vec<String>, PinError> {
        self.ledger.peers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::ferry_pin_testutil::*;

    #[test]
    fn new_and_summary_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = PinManager::new(dir.path());

        assert_eq!(mgr.record().unwrap(), None);
        assert!(!mgr.is_holding().unwrap());

        let summary = mgr.summary().unwrap();
        assert_eq!(summary.state, "none");
        assert!(!summary.holding);
        assert!(summary.paths.is_empty());
        assert_eq!(summary.total_held_paths, 0);
        assert!(summary.held_by_peer.is_empty());
    }

    #[test]
    fn start_and_summary_active() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = PinManager::new(dir.path());
        let pid = std::process::id();
        let dev = "aa".repeat(32);

        let rec = mgr
            .start_session(vec!["src/**".into()], pid, &dev, BTreeMap::new())
            .unwrap();
        assert_eq!(rec.paths, vec!["src/**".to_string()]);
        assert_eq!(rec.pid, pid);

        assert!(mgr.is_holding().unwrap());
        let summary = mgr.summary().unwrap();
        assert_eq!(summary.state, "active");
        assert!(summary.holding);
        assert_eq!(summary.paths, vec!["src/**".to_string()]);
        assert_eq!(summary.pid, Some(pid));
        assert_eq!(summary.device_id, Some(dev));
    }

    #[test]
    fn start_session_bad_pattern_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = PinManager::new(dir.path());
        let err = mgr
            .start_session(
                vec!["[z-a]".into()],
                std::process::id(),
                "dev",
                BTreeMap::new(),
            )
            .unwrap_err();
        assert!(matches!(err, PinError::BadPattern { .. }));
    }

    #[test]
    fn stop_session_marks_released() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = PinManager::new(dir.path());
        assert!(!mgr.stop_session().unwrap());

        mgr.start_session(vec!["*".into()], std::process::id(), "dev", BTreeMap::new())
            .unwrap();
        assert!(mgr.stop_session().unwrap());

        let summary = mgr.summary().unwrap();
        assert_eq!(summary.state, "released");
        assert!(!summary.holding);
        assert!(!mgr.is_holding().unwrap());
    }

    #[test]
    fn held_entries_appear_in_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = PinManager::new(dir.path());
        let p1 = "11".repeat(32);
        let p2 = "22".repeat(32);

        mgr.append_held(
            &p1,
            &[held_entry_for("a.txt", p1.clone(), &"cc".repeat(32))],
        )
        .unwrap();
        mgr.append_held(
            &p2,
            &[
                held_entry_for("b.txt", p2.clone(), &"cc".repeat(32)),
                held_entry_for("c.txt", p2.clone(), &"cc".repeat(32)),
            ],
        )
        .unwrap();

        let summary = mgr.summary().unwrap();
        assert_eq!(summary.total_held_paths, 3);
        assert_eq!(summary.held_by_peer.len(), 2);
        assert_eq!(summary.held_by_peer[&p1], vec!["a.txt".to_string()]);
        assert_eq!(
            summary.held_by_peer[&p2],
            vec!["b.txt".to_string(), "c.txt".to_string()]
        );
    }

    #[test]
    fn release_peer_noops_without_ledger() {
        let rig = Rig::rig_one_file();
        let mgr = PinManager::new(&rig.a_state);
        let peer = ferry_store::format::hex(&rig.b_dev);

        let out = mgr
            .release_peer(
                &peer,
                &rig.a.store,
                &rig.a.tree,
                &rig.local_manifest,
                None,
                (1_787_574_896, 0),
            )
            .unwrap();
        assert_eq!(out.held_entries, 0);

        let plans = mgr.held_peers().unwrap();
        assert!(plans.is_empty());
    }

    #[test]
    fn start_session_with_duration_records_expiration() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = PinManager::new(dir.path());
        let pid = std::process::id();
        let dev = "aa".repeat(32);

        let rec = mgr
            .start_session_with_duration(
                vec!["src/**".into()],
                pid,
                &dev,
                BTreeMap::new(),
                Some(3600),
            )
            .unwrap();
        assert_eq!(rec.expires_sec, Some(rec.started_sec + 3600));
        assert!(mgr.is_holding().unwrap());
    }
}
