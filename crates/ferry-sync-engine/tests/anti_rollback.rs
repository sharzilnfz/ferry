//! Anti-rollback acceptance tests (P0.4).
//!
//! Asserts that a peer restored from an older backup or rolled back to a
//! previous manifest cannot cause a healthy peer to delete untouched files
//! or overwrite newer content with stale versions.

use std::fs;
use std::path::PathBuf;

use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::BlobKind;
use ferry_store::manifest::serialize_manifest;
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use ferry_store::store::Store;
use ferry_sync_engine::ConvergenceEngine;
use rand::SeedableRng;

const DEV_A: [u8; 32] = [0xA1; 32];
const DEV_B: [u8; 32] = [0xB2; 32];
const FOLDER: [u8; 16] = [7; 16];
const SEED: u64 = 42;

struct TestNode {
    _dir: tempfile::TempDir,
    tree: PathBuf,
    state: PathBuf,
    store: Store,
    poly: ferry_store::chunker::ValidatedPoly,
    device_id: [u8; 32],
}

impl TestNode {
    fn new(dev_id: [u8; 32]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("tree");
        let store_dir = dir.path().join(".ferry");
        let state = dir.path().join("state");
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&store_dir).unwrap();
        fs::create_dir_all(&state).unwrap();

        let poly = ferry_store::chunker::ValidatedPoly::generate(
            &mut rand::rngs::StdRng::seed_from_u64(SEED),
        );
        let fmk = [1u8; 32];
        let store = Store::create(&store_dir, fmk, Box::new(PassthroughCipher)).unwrap();
        store.put_polynomial(poly.get()).unwrap();

        Self {
            _dir: dir,
            tree,
            state,
            store,
            poly,
            device_id: dev_id,
        }
    }

    fn write_file(&self, rel: &str, content: &[u8]) {
        let p = self.tree.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    fn snapshot(&self, parent_id: [u8; 32], sec: i64) -> SnapshotOutput {
        snapshot_dir(
            &self.store,
            self.poly,
            &self.tree,
            &SnapshotIdentity {
                folder_id: FOLDER,
                device_id: self.device_id,
                parent_manifest_id: parent_id,
                created_sec: sec,
                created_nsec: 0,
            },
        )
        .unwrap()
    }
}

#[test]
fn test_peer_rollback_does_not_delete_untouched_local_files() {
    let a = TestNode::new(DEV_A);
    let b = TestNode::new(DEV_B);

    // 1. Initial shared state: file1.txt
    a.write_file("file1.txt", b"initial content 1");
    let snap_m1 = a.snapshot([0; 32], 1_000_000);
    let m1_bytes = serialize_manifest(&snap_m1.manifest);
    let m1_id = snap_m1.manifest_id;

    // Both stores know M1
    a.store.put_meta(BlobKind::Manifest, &m1_bytes).unwrap();
    b.store.put_meta(BlobKind::Manifest, &m1_bytes).unwrap();

    // 2. Device A evolves forward: adds file2.txt and file3.txt
    a.write_file("file2.txt", b"important local data 2");
    a.write_file("file3.txt", b"important local data 3");
    let snap_m2 = a.snapshot(m1_id, 2_000_000);
    let m2_bytes = serialize_manifest(&snap_m2.manifest);

    a.store.put_meta(BlobKind::Manifest, &m2_bytes).unwrap();

    // 3. Device B was restored from an older backup (holds M1 only, lacks file2 and file3).
    // B connects to A offering M1.
    // Device A runs convergence with local = M2, remote = M1, base = M2 (from agreement).
    let mut engine = ConvergenceEngine::new(&a.store, &a.tree).state_dir(&a.state);
    let res = engine
        .converge(
            &snap_m2.manifest,
            &snap_m1.manifest,
            Some(&snap_m2.manifest),
        )
        .unwrap();

    // Verification: Zero silent data loss!
    // file2.txt and file3.txt MUST survive on Device A disk
    assert!(a.tree.join("file1.txt").is_file(), "file1.txt must exist");
    assert!(
        a.tree.join("file2.txt").is_file(),
        "file2.txt must not be deleted by rolled-back peer"
    );
    assert!(
        a.tree.join("file3.txt").is_file(),
        "file3.txt must not be deleted by rolled-back peer"
    );
    assert_eq!(
        fs::read(a.tree.join("file2.txt")).unwrap(),
        b"important local data 2"
    );
    assert_eq!(
        fs::read(a.tree.join("file3.txt")).unwrap(),
        b"important local data 3"
    );

    // All local files must be on the send list to peer B to restore consistency
    assert!(
        !res.send.is_empty(),
        "winner files must be sent to rolled-back peer"
    );
}

#[test]
fn test_peer_rollback_does_not_overwrite_newer_file_with_stale_content() {
    let a = TestNode::new(DEV_A);

    // 1. Initial shared state: file1.txt version 1
    a.write_file("file1.txt", b"v1 original");
    let snap_m1 = a.snapshot([0; 32], 1_000_000);
    let m1_bytes = serialize_manifest(&snap_m1.manifest);
    let m1_id = snap_m1.manifest_id;
    a.store.put_meta(BlobKind::Manifest, &m1_bytes).unwrap();

    // 2. A edits file1.txt to version 2
    a.write_file("file1.txt", b"v2 updated live");
    let snap_m2 = a.snapshot(m1_id, 2_000_000);
    let m2_bytes = serialize_manifest(&snap_m2.manifest);
    a.store.put_meta(BlobKind::Manifest, &m2_bytes).unwrap();

    // 3. Remote peer offers stale M1 (created at 1_000_000)
    let mut engine = ConvergenceEngine::new(&a.store, &a.tree).state_dir(&a.state);
    let _res = engine
        .converge(
            &snap_m2.manifest,
            &snap_m1.manifest,
            Some(&snap_m2.manifest),
        )
        .unwrap();

    // Verification: Live file must retain v2 content, not overwritten with stale v1
    assert_eq!(
        fs::read(a.tree.join("file1.txt")).unwrap(),
        b"v2 updated live",
        "stale rolled-back manifest must not overwrite newer local edit"
    );
}
