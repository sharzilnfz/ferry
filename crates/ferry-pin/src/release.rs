




















use std::path::Path;

use ferry_store::format::{unhex, BlobKind};
use ferry_store::manifest::{parse_manifest, RootManifest};
use ferry_store::store::Store;
use ferry_sync_engine::{ConvergenceEngine, ConvergenceError, ConvergenceResult};

use crate::error::PinError;
use crate::held::{distinct_paths, HeldLedger};


#[derive(Debug)]
pub struct ReleasePeerPlan {
    
    pub device_id: String,
    
    
    pub remote_manifest_id: String,
    
    pub held_entries: usize,
    
    pub held_paths: Vec<String>,
    
    pub result: ConvergenceResult,
}

impl ReleasePeerPlan {
    
    pub(crate) fn noop(device_id: String) -> Self {
        ReleasePeerPlan {
            device_id,
            remote_manifest_id: String::new(),
            held_entries: 0,
            held_paths: Vec::new(),
            result: ConvergenceResult::default(),
        }
    }
}





#[allow(clippy::too_many_arguments)]
pub fn release_peer(
    store: &Store,
    root: &Path,
    state_dir: &Path,
    local: &RootManifest,
    peer_hex: &str,
    base: Option<&RootManifest>,
    now: (i64, u32),
) -> Result<ReleasePeerPlan, PinError> {
    let ledger = HeldLedger::new(state_dir);
    let entries = ledger.load_peer(peer_hex)?;
    if entries.is_empty() {
        return Ok(ReleasePeerPlan::noop(peer_hex.to_string()));
    }
    
    let manifest_hex = entries
        .last()
        .expect("non-empty checked above")
        .remote_manifest_id
        .clone();
    let remote = load_manifest(
        store,
        &manifest_hex,
        peer_hex,
        format!("held by peer {peer_hex}"),
    )?;
    let result = ConvergenceEngine::new(store, root)
        .state_dir(state_dir)
        .no_hold()
        .at(now)
        .converge(local, &remote, base)
        .map_err(pin_convergence)?;
    Ok(ReleasePeerPlan {
        device_id: peer_hex.to_string(),
        remote_manifest_id: manifest_hex,
        held_entries: entries.len(),
        held_paths: distinct_paths(&entries),
        result,
    })
}




pub(crate) fn pin_convergence(e: ConvergenceError) -> PinError {
    PinError::Converge(e.to_string())
}

pub(crate) fn load_manifest(
    store: &Store,
    id_hex: &str,
    peer: &str,
    context: String,
) -> Result<RootManifest, PinError> {
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

        
        let err = load_manifest(&rig.a.store, &"cc".repeat(32), &b_hex, "held".into()).unwrap_err();
        assert!(matches!(err, PinError::ManifestMissing { .. }), "{err}");
    }

    #[test]
    fn no_ledger_means_an_empty_noop_release() {
        let rig = Rig::rig_one_file();
        let peer = ferry_store::format::hex(&rig.b_dev);
        let out = release_peer(
            &rig.a.store,
            &rig.a.tree,
            &rig.a_state,
            &rig.local_manifest,
            &peer,
            None,
            (1_787_574_896, 0),
        )
        .unwrap();
        assert_eq!(out.held_entries, 0);
        assert!(out.result.is_noop());
    }
}




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
        
        #[allow(dead_code)]
        dir: tempfile::TempDir,
        pub a: DeviceParts,
        pub b_dev: [u8; 32],
        pub a_state: PathBuf,
        pub local_manifest: ferry_store::manifest::RootManifest,
    }

    pub struct DeviceParts {
        pub store: Store,
        pub tree: PathBuf,
    }

    pub fn fmk() -> [u8; KEY_LEN] {
        core::array::from_fn(|i| (i * 17 + 3) as u8)
    }

    pub fn poly_of(seed: u64) -> ferry_store::chunker::ValidatedPoly {
        ferry_store::chunker::ValidatedPoly::generate(&mut StdRng::seed_from_u64(seed))
    }

    impl Rig {
        
        
        pub fn rig_one_file() -> Rig {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            let mk = |tag: u8| {
                let tree = root.join(format!("tree-{tag}"));
                std::fs::create_dir_all(&tree).unwrap();
                let store_root = root.join(format!("store-{tag}"));
                
                
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
