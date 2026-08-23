//! Test fixtures shared by unit tests in this crate: two-device setups with
//! real stores, trees, and snapshots. Integration tests (tests/) carry their
//! own copies because this module is cfg(test)-only.

#![cfg(test)]

use ferry_store::chunker::generate_polynomial;
use ferry_store::crypto::{PassthroughCipher, KEY_LEN};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use ferry_store::store::Store;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::path::{Path, PathBuf};

pub fn fmk() -> [u8; KEY_LEN] {
    core::array::from_fn(|i| (i * 11 + 5) as u8)
}

pub fn fresh_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path(), fmk(), Box::new(PassthroughCipher)).unwrap();
    (dir, store)
}

pub fn poly_of(seed: u64) -> u64 {
    generate_polynomial(&mut StdRng::seed_from_u64(seed))
}

pub fn identity(device_id: [u8; 32], at: (i64, u32), parent: [u8; 32]) -> SnapshotIdentity {
    SnapshotIdentity {
        folder_id: [7; 16],
        device_id,
        parent_manifest_id: parent,
        created_sec: at.0,
        created_nsec: at.1,
    }
}

pub fn write_file(path: &Path, bytes: &[u8], exec: bool, mt: (i64, u32)) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
    let mut perm = std::fs::metadata(path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(if exec { 0o755 } else { 0o644 });
    std::fs::set_permissions(path, perm).unwrap();
    set_mtime(path, mt.0, mt.1);
}

pub fn set_mtime(path: &Path, sec: i64, nsec: u32) {
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(
        std::time::UNIX_EPOCH + std::time::Duration::new(sec as u64, nsec),
    ))
    .unwrap();
}

/// Directories need a read-only handle for futimens.
pub fn set_dir_mtime(path: &Path, sec: i64, nsec: u32) {
    let f = std::fs::File::open(path).unwrap();
    f.set_times(std::fs::FileTimes::new().set_modified(
        std::time::UNIX_EPOCH + std::time::Duration::new(sec as u64, nsec),
    ))
    .unwrap();
}

pub struct Device {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub tree: PathBuf,
    pub state_dir: PathBuf,
    pub device_id: [u8; 32],
    pub poly: u64,
    pub parent: [u8; 32],
    pub clock: i64,
}

impl Device {
    /// A device rooted in its own temp dir: `store/`, `tree/`, `state/`.
    pub fn new(tag: u8, device_id: [u8; 32], poly: u64) -> Device {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let store_root = root.join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = Store::create(&store_root, fmk(), Box::new(PassthroughCipher)).unwrap();
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        Device {
            dir,
            store,
            tree,
            state_dir: root.join("state"),
            device_id,
            poly,
            parent: [0; 32],
            clock: 1_787_000_000 + tag as i64,
        }
    }

    pub fn snapshot(&mut self) -> SnapshotOutput {
        self.clock += 1;
        let idn = identity(self.device_id, (self.clock, 0), self.parent);
        let out = snapshot_dir(&self.store, self.poly, &self.tree, &idn).unwrap();
        self.parent = out.manifest_id;
        out
    }
}

/// Simulate the transport: copy blobs from one store to another.
pub fn transfer(from: &Store, to: &Store, ids: &[(ferry_store::format::BlobKind, ferry_store::format::BlobId)]) {
    for (kind, id) in ids {
        if to.get(*kind, id).is_err() {
            let bytes = from.get(*kind, id).expect("source blob missing");
            to.put_blob(*kind, &bytes).expect("put into target store");
        }
    }
}

/// Copy a manifest and its whole tree-node closure between stores
/// (metadata-first exchange).
pub fn transfer_manifest(
    from: &Store,
    to: &Store,
    manifest: &ferry_store::manifest::RootManifest,
    manifest_id: ferry_store::format::BlobId,
) {
    use ferry_store::format::BlobKind;
    transfer(from, to, &[(BlobKind::Manifest, manifest_id)]);
    let mut stack = vec![manifest.root_tree_id];
    while let Some(id) = stack.pop() {
        if to.get(BlobKind::TreeNode, &id).is_ok() {
            continue;
        }
        transfer(from, to, &[(BlobKind::TreeNode, id)]);
        let node = ferry_store::manifest::parse_tree_node(&to.get(BlobKind::TreeNode, &id).unwrap())
            .unwrap();
        for e in node.entries {
            if let ferry_store::manifest::EntryPayload::Dir { child_tree_id } = e.payload {
                stack.push(child_tree_id);
            }
        }
    }
}
