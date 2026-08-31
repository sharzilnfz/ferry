


use std::collections::BTreeMap;
use std::path::Path;

use ferry_sync_engine::pin::PinManager;
use ferry_store::agreement::{AgreedRecord, AgreementLedger};
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::manifest::{parse_manifest, parse_tree_node, EntryPayload};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use ferry_store::store::Store;
use ferry_sync_engine::report::list_conflicts;
use ferry_sync_engine::{ConvergenceEngine, ConvergenceError};
use rand::SeedableRng;

const DEV_A: [u8; 32] = [0xA1; 32];
const DEV_B: [u8; 32] = [0xB2; 32];
const FOLDER: [u8; 16] = [9; 16];
const NOW: (i64, u32) = (1_787_574_896, 0);

fn fmk() -> [u8; 32] {
    core::array::from_fn(|i| (i * 13 + 1) as u8)
}

fn poly(seed: u64) -> ferry_store::chunker::ValidatedPoly {
    ferry_store::chunker::ValidatedPoly::generate(&mut rand::rngs::StdRng::seed_from_u64(seed))
}

fn write_file(path: &Path, bytes: &[u8], mt: (i64, u32)) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(
        std::fs::FileTimes::new()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::new(mt.0 as u64, mt.1)),
    )
    .unwrap();
}

fn snap(store: &Store, tree: &Path, dev: [u8; 32], parent: [u8; 32], clock: i64) -> SnapshotOutput {
    let idn = SnapshotIdentity {
        folder_id: FOLDER,
        device_id: dev,
        parent_manifest_id: parent,
        created_sec: clock,
        created_nsec: 0,
    };
    snapshot_dir(store, poly(42), tree, &idn).unwrap()
}

fn transfer_meta(from: &Store, to: &Store, s: &SnapshotOutput) {
    if to.get(BlobKind::Manifest, &s.manifest_id).is_err() {
        let b = from.get(BlobKind::Manifest, &s.manifest_id).unwrap();
        to.put_blob(BlobKind::Manifest, &b).unwrap();
    }
    let mut stack = vec![s.root_tree_id];
    while let Some(id) = stack.pop() {
        if to.get(BlobKind::TreeNode, &id).is_ok() {
            continue;
        }
        let b = from.get(BlobKind::TreeNode, &id).unwrap();
        to.put_blob(BlobKind::TreeNode, &b).unwrap();
        let node = parse_tree_node(&to.get(BlobKind::TreeNode, &id).unwrap()).unwrap();
        for e in node.entries {
            if let EntryPayload::Dir { child_tree_id } = e.payload {
                stack.push(child_tree_id);
            }
        }
    }
}

fn transfer_chunks(from: &Store, to: &Store, ids: &[(BlobId, u64)]) {
    for (id, _) in ids {
        if to.get(BlobKind::DataChunk, id).is_err() {
            let b = from
                .get(BlobKind::DataChunk, id)
                .expect("peer must hold advertised chunk");
            to.put_blob(BlobKind::DataChunk, &b).unwrap();
        }
    }
}

struct PeerFetch<'x> {
    from: &'x Store,
    to: &'x Store,
}

impl ferry_sync_engine::BlobFetch for PeerFetch<'_> {
    fn fetch(&mut self, want: &[(BlobId, u64)]) -> Result<(), ConvergenceError> {
        transfer_chunks(self.from, self.to, want);
        Ok(())
    }
}

#[test]
fn held_ledger_persists_across_daemon_crashes_and_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let a_store_dir = root.join("a_store");
    let a_tree = root.join("a_tree");
    let a_state = root.join("a_state");
    let b_store_dir = root.join("b_store");
    let b_tree = root.join("b_tree");

    std::fs::create_dir_all(&a_store_dir).unwrap();
    std::fs::create_dir_all(&a_tree).unwrap();
    std::fs::create_dir_all(&a_state).unwrap();
    std::fs::create_dir_all(&b_store_dir).unwrap();
    std::fs::create_dir_all(&b_tree).unwrap();

    let a_store = Store::create(&a_store_dir, fmk(), Box::new(PassthroughCipher)).unwrap();
    let b_store = Store::create(&b_store_dir, fmk(), Box::new(PassthroughCipher)).unwrap();

    
    write_file(&a_tree.join("src/lib.rs"), b"v0-lib", (1000, 0));
    write_file(&a_tree.join("src/main.rs"), b"v0-main", (1000, 0));
    write_file(&a_tree.join("docs/readme.md"), b"v0-doc", (1000, 0));

    write_file(&b_tree.join("src/lib.rs"), b"v0-lib", (1000, 0));
    write_file(&b_tree.join("src/main.rs"), b"v0-main", (1000, 0));
    write_file(&b_tree.join("docs/readme.md"), b"v0-doc", (1000, 0));

    let sa_base = snap(&a_store, &a_tree, DEV_A, [0; 32], 1000);
    let sb_base = snap(&b_store, &b_tree, DEV_B, [0; 32], 1000);
    transfer_meta(&a_store, &b_store, &sa_base);
    transfer_meta(&b_store, &a_store, &sb_base);

    AgreementLedger::new(&a_state)
        .record(
            &FOLDER,
            &AgreedRecord {
                peer_device_id: DEV_B,
                manifest_id: sa_base.manifest_id,
                agreed_sec: 1000,
                agreed_nsec: 0,
            },
        )
        .unwrap();

    
    let mut base_agreements = BTreeMap::new();
    base_agreements.insert(hex(&DEV_B), hex(&sa_base.manifest_id));

    let pin_mgr = PinManager::new(&a_state);
    let pin_rec = pin_mgr
        .start_session(
            vec!["src/**".into()],
            std::process::id(),
            &hex(&DEV_A),
            base_agreements,
        )
        .unwrap();
    assert!(pin_rec.holding());

    
    write_file(&a_tree.join("src/lib.rs"), b"v1-a-lib", (3000, 0));
    write_file(&a_tree.join("src/main.rs"), b"v1-a-main", (2500, 0));

    write_file(&b_tree.join("src/lib.rs"), b"v1-b-lib", (2900, 0));
    write_file(&b_tree.join("src/main.rs"), b"v1-b-main", (3000, 0));
    write_file(&b_tree.join("docs/readme.md"), b"v1-b-doc", (2800, 0));

    let sa_edit = snap(&a_store, &a_tree, DEV_A, sa_base.manifest_id, 2000);
    let sb_edit = snap(&b_store, &b_tree, DEV_B, sb_base.manifest_id, 2000);
    transfer_meta(&b_store, &a_store, &sb_edit);

    let remote_manifest = parse_manifest(
        &a_store
            .get(BlobKind::Manifest, &sb_edit.manifest_id)
            .unwrap(),
    )
    .unwrap();

    
    let mut fetch = PeerFetch {
        from: &b_store,
        to: &a_store,
    };
    let result = ConvergenceEngine::new(&a_store, &a_tree)
        .state_dir(&a_state)
        .at(NOW)
        .fetch_with(&mut fetch)
        .converge(&sa_edit.manifest, &remote_manifest, Some(&sa_base.manifest))
        .unwrap();

    assert_eq!(
        result.held.len(),
        3,
        "src, src/lib.rs, and src/main.rs are held"
    );
    assert_eq!(
        std::fs::read(a_tree.join("docs/readme.md")).unwrap(),
        b"v1-b-doc"
    );
    assert_eq!(
        std::fs::read(a_tree.join("src/lib.rs")).unwrap(),
        b"v1-a-lib"
    );

    
    a_store.flush().unwrap();
    b_store.flush().unwrap();
    drop(a_store);
    drop(b_store);
    drop(pin_mgr);

    
    let pin_mgr_restarted = PinManager::new(&a_state);
    let summary = pin_mgr_restarted.summary().unwrap();
    assert_eq!(summary.state, "active");
    assert!(summary.holding);
    assert_eq!(summary.total_held_paths, 3);
    assert_eq!(
        summary.held_by_peer.get(&hex(&DEV_B)).unwrap(),
        &vec![
            "src".to_string(),
            "src/lib.rs".to_string(),
            "src/main.rs".to_string()
        ]
    );

    let loaded_entries = pin_mgr_restarted.load_held_peer(&hex(&DEV_B)).unwrap();
    assert_eq!(loaded_entries.len(), 3);
    for e in &loaded_entries {
        assert_eq!(e.device_id, hex(&DEV_B));
        assert_eq!(e.remote_manifest_id, hex(&sb_edit.manifest_id));
    }

    
    drop(pin_mgr_restarted);

    let a_store_restarted = Store::open(&a_store_dir, fmk(), Box::new(PassthroughCipher)).unwrap();
    let pin_mgr_final = PinManager::new(&a_state);

    let sa_prescan = snap(
        &a_store_restarted,
        &a_tree,
        DEV_A,
        sa_edit.manifest_id,
        3500,
    );

    let release_summary = pin_mgr_final
        .release(&a_store_restarted, &a_tree, &sa_prescan.manifest, NOW)
        .unwrap();

    assert_eq!(release_summary.peers.len(), 1);
    let p_res = &release_summary.peers[0];
    assert_eq!(p_res.device_id, hex(&DEV_B));
    assert_eq!(p_res.held_entries, 3);
    assert_eq!(p_res.held_paths, vec!["src", "src/lib.rs", "src/main.rs"]);
    assert_eq!(release_summary.total_conflicts, 2);
    assert!(release_summary.pin_ended);

    
    
    
    assert_eq!(
        std::fs::read(a_tree.join("src/lib.rs")).unwrap(),
        b"v1-a-lib"
    );
    assert_eq!(
        std::fs::read(a_tree.join("src/main.rs")).unwrap(),
        b"v1-b-main"
    );

    
    let conflicts = list_conflicts(&a_state).unwrap();
    assert_eq!(conflicts.len(), 3);

    
    assert!(pin_mgr_final.held_peers().unwrap().is_empty());
    assert!(!pin_mgr_final.is_holding().unwrap());

    
    drop(pin_mgr_final);
    drop(a_store_restarted);

    let fresh_mgr = PinManager::new(&a_state);
    let fresh_store = Store::open(&a_store_dir, fmk(), Box::new(PassthroughCipher)).unwrap();
    let final_scan = snap(&fresh_store, &a_tree, DEV_A, sa_prescan.manifest_id, 4000);

    let noop_summary = fresh_mgr
        .release(&fresh_store, &a_tree, &final_scan.manifest, NOW)
        .unwrap();
    assert!(noop_summary.peers.is_empty());
    assert_eq!(noop_summary.total_conflicts, 0);
    assert_eq!(noop_summary.total_ops, 0);
}

#[test]
fn failed_release_preserves_held_ledger_for_restart_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let a_store_dir = root.join("a_store");
    let a_tree = root.join("a_tree");
    let a_state = root.join("a_state");

    std::fs::create_dir_all(&a_store_dir).unwrap();
    std::fs::create_dir_all(&a_tree).unwrap();
    std::fs::create_dir_all(&a_state).unwrap();

    let a_store = Store::create(&a_store_dir, fmk(), Box::new(PassthroughCipher)).unwrap();

    
    write_file(&a_tree.join("src/lib.rs"), b"a-initial", (1000, 0));
    let sa = snap(&a_store, &a_tree, DEV_A, [0; 32], 1000);

    
    let pin_mgr = PinManager::new(&a_state);
    let mut base_agreements = BTreeMap::new();
    base_agreements.insert(hex(&DEV_B), hex(&sa.manifest_id));
    pin_mgr
        .start_session(
            vec!["src/**".into()],
            std::process::id(),
            &hex(&DEV_A),
            base_agreements,
        )
        .unwrap();

    let fake_remote_manifest = "ee".repeat(32);
    let held_entry = ferry_sync_engine::pin::HeldEntry {
        held_sec: 1500,
        held_nsec: 0,
        path: "src/lib.rs".to_string(),
        device_id: hex(&DEV_B),
        remote_manifest_id: fake_remote_manifest.clone(),
        chunks: vec![],
        decision: "conflict".to_string(),
        conflict_winner: Some("remote".to_string()),
    };
    pin_mgr.append_held(&hex(&DEV_B), &[held_entry]).unwrap();

    
    let err = pin_mgr
        .release(&a_store, &a_tree, &sa.manifest, NOW)
        .unwrap_err();
    assert!(matches!(err, ferry_sync_engine::pin::PinError::ManifestMissing { .. }));

    
    let peers = pin_mgr.held_peers().unwrap();
    assert_eq!(peers, vec![hex(&DEV_B)]);
    let held = pin_mgr.load_held_peer(&hex(&DEV_B)).unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].path, "src/lib.rs");
    assert_eq!(held[0].remote_manifest_id, fake_remote_manifest);

    
    drop(pin_mgr);
    drop(a_store);

    let restarted_mgr = PinManager::new(&a_state);
    let restarted_peers = restarted_mgr.held_peers().unwrap();
    assert_eq!(restarted_peers, vec![hex(&DEV_B)]);
    let restarted_held = restarted_mgr.load_held_peer(&hex(&DEV_B)).unwrap();
    assert_eq!(restarted_held.len(), 1);
    assert_eq!(restarted_held[0].path, "src/lib.rs");
}
