//! Release: turn a held set back into ordinary three-way reconciliation.
//!
//! Per peer with a ledger:
//!
//! 1. The LATEST held entry names the freshest remote manifest that arrived
//!    during the pin (its bytes were fetched during the hold, so the peer
//!    need not be online).
//! 2. The three-way base is the agreement captured at PIN START
//!    (`base_agreements` in the pin record) — exactly "last-agreed before
//!    pin". A peer unknown at start reconciles against an empty ancestor.
//! 3. [`ferry_sync_engine::reconcile`] decides every path; the returned
//!    plan is executed by the CALLER through the ordinary engine, so
//!    outcomes are exactly ADR-0004 outcomes: winner live, loser
//!    quarantined as `path.ferry-conflict.<loser>-<ts>` plus a report
//!    entry. Nothing is merged, nothing is lost.
//!
//! Idempotence: after the release plan runs and the ledgers clear, a second
//! release finds no peers → empty plan list → a no-op. Re-running before
//! clearing (e.g. after an execution error) recomputes the same decisions;
//! quarantine name collision counters make re-execution safe.

use std::collections::BTreeMap;

use ferry_store::format::{unhex, BlobKind};
use ferry_store::manifest::parse_manifest;
use ferry_store::store::Store;
use ferry_sync_engine::reconcile::{reconcile, ReconcileInput};
use ferry_sync_engine::ActionPlan;

use crate::error::PinError;
use crate::held::{distinct_paths, HeldLedger};

/// One peer's reconstructed release plan.
#[derive(Debug)]
pub struct ReleasePeerPlan {
    /// Peer device id (64 lowercase hex) — also its ledger file stem.
    pub device_id: String,
    /// Manifest id (hex) whose changes were held; the plan reconciles
    /// against this as the remote side.
    pub remote_manifest_id: String,
    /// Ledger lines seen for this peer.
    pub held_entries: usize,
    /// Distinct held paths, sorted.
    pub held_paths: Vec<String>,
    /// The executable three-way plan (run via `ferry_sync_engine::execute`).
    pub plan: ActionPlan,
}

/// Build release plans for every peer with a ledger under `state_dir`.
///
/// Pure: reads stores/ledgers, writes nothing. The caller executes each
/// plan, then clears ledgers ([`HeldLedger::clear_peer`]) and marks the pin
/// released — in that order, so a failed execution leaves everything
/// retryable.
pub fn plan_release(
    store: &Store,
    local: &ferry_store::manifest::RootManifest,
    bases: &BTreeMap<String, String>,
    ledger: &HeldLedger,
) -> Result<Vec<ReleasePeerPlan>, PinError> {
    let mut out = Vec::new();
    for peer in ledger.peers()? {
        let entries = ledger.load_peer(&peer)?;
        if entries.is_empty() {
            continue;
        }
        // Latest arrival wins: later entries carry fresher peer manifests.
        let manifest_hex = entries
            .last()
            .expect("non-empty checked above")
            .remote_manifest_id
            .clone();
        let remote = load_manifest(store, &manifest_hex, &peer, format!("held by peer {peer}"))?;
        let base = match bases.get(&peer) {
            Some(base_hex) => Some(load_manifest(
                store,
                base_hex,
                &peer,
                "captured as last-agreed at pin start".to_string(),
            )?),
            None => None,
        };
        let plan = reconcile(ReconcileInput {
            store,
            local,
            remote: &remote,
            base: base.as_ref(),
        })?;
        out.push(ReleasePeerPlan {
            device_id: peer.clone(),
            remote_manifest_id: manifest_hex,
            held_entries: entries.len(),
            held_paths: distinct_paths(&entries),
            plan,
        });
    }
    Ok(out)
}

pub(crate) fn load_manifest(
    store: &Store,
    id_hex: &str,
    peer: &str,
    context: String,
) -> Result<ferry_store::manifest::RootManifest, PinError> {
    let id = unhex::<32>(id_hex).ok_or_else(|| PinError::ManifestMissing {
        peer: peer.to_string(),
        manifest_id: context.clone(),
    })?;
    let bytes = store
        .get(BlobKind::Manifest, &id)
        .map_err(|_| PinError::ManifestMissing {
            peer: peer.to_string(),
            manifest_id: format!("{id_hex} ({context})"),
        })?;
    Ok(parse_manifest(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferry_pin_testutil::*;

    #[test]
    fn missing_held_manifest_is_a_loud_error_naming_peer_and_id() {
        let rig = Rig::rig_one_file();
        let b_hex = ferry_store::format::hex(&rig.b_dev);
        let mut bases = BTreeMap::new();
        bases.insert(
            b_hex.clone(),
            "1111111111111111111111111111111111111111111111111111111111111111".to_string(),
        );
        let ledger = HeldLedger::new(&rig.a_state);
        let entries = vec![held_entry_for("f.txt", b_hex.clone(), &"cc".repeat(32))];
        ledger.append(&b_hex, &entries).unwrap();

        let err = plan_release(&rig.a.store, &rig.local_manifest, &bases, &ledger).unwrap_err();
        assert!(matches!(err, PinError::ManifestMissing { .. }), "{err}");
    }

    #[test]
    fn no_ledgers_means_an_empty_noop_plan_list() {
        let rig = Rig::rig_one_file();
        let ledger = HeldLedger::new(&rig.a_state);
        let plans =
            plan_release(&rig.a.store, &rig.local_manifest, &BTreeMap::new(), &ledger).unwrap();
        assert!(plans.is_empty(), "second release is a no-op");
    }
}

/// Shared fixture for this file's unit tests: one tiny two-device rig.
/// (The acceptance integration test in tests/ carries its own fuller
/// harness because cfg(test) items are not visible across crate lines.)
#[cfg(test)]
pub(crate) mod ferry_pin_testutil {
    use ferry_store::crypto::{PassthroughCipher, KEY_LEN};
    use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};
    use ferry_store::store::Store;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use std::path::PathBuf;

    use crate::held::{HeldChunk, HeldEntry};

    pub const A_DEV: [u8; 32] = [0xA1; 32];
    pub const B_DEV: [u8; 32] = [0xB2; 32];

    pub struct Rig {
        /// Holds the temp root alive for every path/store in the rig.
        #[allow(dead_code)]
        dir: tempfile::TempDir,
        pub a: DeviceParts,
        pub b_dev: [u8; 32],
        pub a_state: PathBuf,
        pub local_manifest: ferry_store::manifest::RootManifest,
    }

    pub struct DeviceParts {
        pub store: Store,
        /// Kept for rig readability; the unit tests only exercise stores.
        #[allow(dead_code)]
        pub tree: PathBuf,
    }

    pub fn fmk() -> [u8; KEY_LEN] {
        core::array::from_fn(|i| (i * 17 + 3) as u8)
    }

    pub fn poly_of(seed: u64) -> ferry_store::chunker::ValidatedPoly {
        ferry_store::chunker::ValidatedPoly::generate(&mut StdRng::seed_from_u64(seed))
    }

    impl Rig {
        /// Two seeded devices with identical f.txt, both snapshotted; A's
        /// manifest is the release-time local input.
        pub fn rig_one_file() -> Rig {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            let mk = |tag: u8| {
                let tree = root.join(format!("tree-{tag}"));
                std::fs::create_dir_all(&tree).unwrap();
                let store_root = root.join(format!("store-{tag}"));
                // Store::create makes `<root>/.ferry` with create_dir, so
                // the parent must exist first.
                std::fs::create_dir_all(&store_root).unwrap();
                let store = Store::create(&store_root, fmk(), Box::new(PassthroughCipher)).unwrap();
                (tree, store)
            };
            let (a_tree, a_store) = mk(1);
            let (b_tree, b_store) = mk(2);
            std::fs::write(a_tree.join("f.txt"), b"same").unwrap();
            std::fs::write(b_tree.join("f.txt"), b"same").unwrap();
            let idn = |dev| SnapshotIdentity {
                folder_id: [7; 16],
                device_id: dev,
                parent_manifest_id: [0; 32],
                created_sec: 1_787_000_000,
                created_nsec: 0,
            };
            let sa = snapshot_dir(&a_store, poly_of(3), &a_tree, &idn(A_DEV)).unwrap();
            snapshot_dir(&b_store, poly_of(3), &b_tree, &idn(B_DEV)).unwrap();
            Rig {
                dir,
                a: DeviceParts {
                    store: a_store,
                    tree: a_tree,
                },
                b_dev: B_DEV,
                a_state: root.join("state-a"),
                local_manifest: sa.manifest,
            }
        }
    }

    pub fn held_entry_for(path: &str, peer: String, manifest_hex: &str) -> HeldEntry {
        HeldEntry {
            held_sec: 1_787_574_000,
            held_nsec: 0,
            path: path.to_string(),
            device_id: peer,
            remote_manifest_id: manifest_hex.to_string(),
            chunks: vec![HeldChunk {
                id: "dd".repeat(32),
                len: 4,
            }],
            decision: "conflict".into(),
            conflict_winner: Some("local".into()),
        }
    }
}
