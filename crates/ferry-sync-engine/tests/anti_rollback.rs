//! Anti-rollback acceptance tests (P0.4).
//!
//! Asserts that a peer restored from an older backup or rolled back to a
//! previous manifest cannot cause a healthy peer to delete untouched files
//! or overwrite newer content with stale versions.

use std::fs;
use std::path::PathBuf;

use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::BlobKind;
use ferry_store::manifest::{parse_tree_node, serialize_manifest, EntryPayload};
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

/// Copy a snapshot's manifest, tree-node closure, and data chunks between
/// stores, simulating the metadata-first transport.
fn transfer_snapshot(from: &Store, to: &Store, out: &SnapshotOutput) {
    if to.get(BlobKind::Manifest, &out.manifest_id).is_err() {
        let bytes = from.get(BlobKind::Manifest, &out.manifest_id).unwrap();
        to.put_blob(BlobKind::Manifest, &bytes).unwrap();
    }
    let mut stack = vec![out.manifest.root_tree_id];
    while let Some(id) = stack.pop() {
        if to.get(BlobKind::TreeNode, &id).is_ok() {
            continue;
        }
        let bytes = from.get(BlobKind::TreeNode, &id).unwrap();
        to.put_blob(BlobKind::TreeNode, &bytes).unwrap();
        let node = parse_tree_node(&bytes).unwrap();
        for e in node.entries {
            match e.payload {
                EntryPayload::Dir { child_tree_id } => stack.push(child_tree_id),
                EntryPayload::File { chunks, .. } => {
                    for (cid, _) in chunks {
                        if to.get(BlobKind::DataChunk, &cid).is_err() {
                            let cb = from.get(BlobKind::DataChunk, &cid).unwrap();
                            to.put_blob(BlobKind::DataChunk, &cb).unwrap();
                        }
                    }
                }
                EntryPayload::Symlink { .. } => {}
            }
        }
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

#[test]
fn test_local_rollback_preserves_restored_local_files() {
    let a = TestNode::new(DEV_A);
    let b = TestNode::new(DEV_B);

    // 1. Shared base M1: file1.txt v1, known to both devices.
    a.write_file("file1.txt", b"base v1");
    let snap_m1 = a.snapshot([0; 32], 1_000_000);
    let m1_bytes = serialize_manifest(&snap_m1.manifest);
    let m1_id = snap_m1.manifest_id;
    a.store.put_meta(BlobKind::Manifest, &m1_bytes).unwrap();
    b.store.put_meta(BlobKind::Manifest, &m1_bytes).unwrap();

    // 2. Device B evolves forward: M2 (parent M1) edits file1 and adds file2.
    b.write_file("file1.txt", b"remote v2");
    b.write_file("file2.txt", b"remote addition");
    let snap_m2 = b.snapshot(m1_id, 2_000_000);
    transfer_snapshot(&b.store, &a.store, &snap_m2);

    // 3. Device A was restored from a backup and re-snapshotted WITHOUT
    //    parent linkage: its parent chain never reaches M1, so the base is
    //    provable for B but not for A. The restored file1 holds a local
    //    edit that exists nowhere else.
    a.write_file("file1.txt", b"restored local edit");
    let snap_local = a.snapshot([0; 32], 3_000_000);

    let mut engine = ConvergenceEngine::new(&a.store, &a.tree).state_dir(&a.state);
    let res = engine
        .converge(
            &snap_local.manifest,
            &snap_m2.manifest,
            Some(&snap_m1.manifest),
        )
        .unwrap();

    // The restored local edit survives verbatim: broken local lineage must
    // never diff the local tree against the remote manifest.
    assert_eq!(
        fs::read(a.tree.join("file1.txt")).unwrap(),
        b"restored local edit",
        "locally restored file must survive its own broken lineage"
    );
    // The remote addition still lands on disk.
    assert_eq!(
        fs::read(a.tree.join("file2.txt")).unwrap(),
        b"remote addition"
    );
    // Local files are preserved in the plan: the restored edit ships to the
    // peer instead of being discarded.
    assert!(
        !res.send.is_empty(),
        "restored local content must be on the send list"
    );
}

#[test]
fn test_broken_lineage_on_both_sides_degrades_to_empty_base() {
    let a = TestNode::new(DEV_A);
    let b = TestNode::new(DEV_B);

    // 1. A stale "agreed" base whose empty tree matches neither side.
    let snap_base = a.snapshot([0; 32], 500_000);

    // 2. Both devices restored from unrelated backups: neither manifest's
    //    parent chain reaches the base.
    a.write_file("local-only.txt", b"a data");
    let snap_a = a.snapshot([0; 32], 1_000_000);
    b.write_file("remote-only.txt", b"b data");
    let snap_b = b.snapshot([0; 32], 2_000_000);
    transfer_snapshot(&b.store, &a.store, &snap_b);

    let mut engine = ConvergenceEngine::new(&a.store, &a.tree).state_dir(&a.state);
    let res = engine
        .converge(
            &snap_a.manifest,
            &snap_b.manifest,
            Some(&snap_base.manifest),
        )
        .unwrap();

    // Empty-base degradation: every file on both sides survives as an
    // addition; nothing is pruned.
    assert_eq!(fs::read(a.tree.join("local-only.txt")).unwrap(), b"a data");
    assert_eq!(fs::read(a.tree.join("remote-only.txt")).unwrap(), b"b data");
    assert!(
        res.conflicts.is_empty(),
        "disjoint additions never conflict"
    );
    assert!(
        !res.send.is_empty(),
        "local additions must ship to the peer"
    );
}
