//! Unified high-level manager for session pinning, held ledgers, and release.
//!
//! [`PinManager`] encapsulates [`PinStore`], [`HeldLedger`], [`PathMatcher`],
//! and release planning behind a cohesive interface. Daemon status, CLI commands,
//! and sync engines interact with pin state through `PinManager` rather than
//! manually coordinating procedural primitives.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ferry_store::format::hex as hex_str;
use ferry_store::manifest::RootManifest;
use ferry_store::store::Store;
use serde::{Deserialize, Serialize};

use crate::error::PinError;
use crate::held::{distinct_paths, HeldEntry, HeldLedger};
use crate::matcher::PathMatcher;
use crate::pin::{PinRecord, PinStore, PIN_FORMAT_VERSION};
use crate::release::{plan_release, ReleasePeerPlan};

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

    /// Plan release reconciliation for a specific peer.
    pub fn release_peer(
        &self,
        peer: &[u8; 32],
        store: &Store,
        agreed_base: Option<&RootManifest>,
        local_manifest: &RootManifest,
    ) -> Result<ReleasePeerPlan, PinError> {
        let peer_hex = hex_str(peer);
        let entries = self.ledger.load_peer(&peer_hex)?;
        if entries.is_empty() {
            return Ok(ReleasePeerPlan {
                device_id: peer_hex,
                remote_manifest_id: String::new(),
                held_entries: 0,
                held_paths: Vec::new(),
                plan: ferry_sync_engine::ActionPlan::default(),
            });
        }
        let manifest_hex = entries
            .last()
            .expect("non-empty checked above")
            .remote_manifest_id
            .clone();
        let remote = crate::release::load_manifest(
            store,
            &manifest_hex,
            &peer_hex,
            format!("held by peer {peer_hex}"),
        )?;
        let base_owned = match agreed_base {
            Some(_) => None,
            None => {
                let rec = self.store.load()?;
                if let Some(r) = rec {
                    if let Some(b_hex) = r.base_agreements.get(&peer_hex) {
                        Some(crate::release::load_manifest(
                            store,
                            b_hex,
                            &peer_hex,
                            "captured as last-agreed at pin start".to_string(),
                        )?)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        };
        let effective_base = agreed_base.or(base_owned.as_ref());
        let plan = ferry_sync_engine::reconcile::reconcile(
            ferry_sync_engine::reconcile::ReconcileInput {
                store,
                local: local_manifest,
                remote: &remote,
                base: effective_base,
            },
        )?;
        Ok(ReleasePeerPlan {
            device_id: peer_hex,
            remote_manifest_id: manifest_hex,
            held_entries: entries.len(),
            held_paths: distinct_paths(&entries),
            plan,
        })
    }

    /// Build release plans for every peer with a held ledger.
    pub fn plan_release(
        &self,
        store: &Store,
        local_manifest: &RootManifest,
    ) -> Result<Vec<ReleasePeerPlan>, PinError> {
        let bases = match self.store.load()? {
            Some(rec) => rec.base_agreements,
            None => BTreeMap::new(),
        };
        plan_release(store, local_manifest, &bases, &self.ledger)
    }

    /// Clear one peer's held ledger after successful release.
    pub fn clear_peer(&self, peer_hex: &str) -> Result<bool, PinError> {
        self.ledger.clear_peer(peer_hex)
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
    fn release_peer_and_plan_release() {
        let rig = Rig::rig_one_file();
        let mgr = PinManager::new(&rig.a_state);

        // Empty release
        let empty_plan = mgr
            .release_peer(&rig.b_dev, &rig.a.store, None, &rig.local_manifest)
            .unwrap();
        assert_eq!(empty_plan.held_entries, 0);

        let plans = mgr.plan_release(&rig.a.store, &rig.local_manifest).unwrap();
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
