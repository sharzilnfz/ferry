//! Test helpers shared by unit tests in this crate. Not compiled outside
//! `cargo test`; integration tests under `tests/` keep their own copies.

#![cfg(test)]

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use std::fs::FileTimes;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use ferry_store::crypto::{PassthroughCipher, KEY_LEN};
use ferry_store::snapshot::SnapshotIdentity;
use ferry_store::store::Store;

pub fn fmk() -> [u8; KEY_LEN] {
    core::array::from_fn(|i| i as u8)
}

/// Fresh store rooted at its own tempdir.
pub fn fresh_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::create(dir.path(), fmk(), Box::new(PassthroughCipher)).unwrap();
    (dir, store)
}

pub fn poly_of(seed: u64) -> u64 {
    ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(seed))
}

pub fn identity(at: (i64, u32)) -> SnapshotIdentity {
    SnapshotIdentity {
        folder_id: [7; 16],
        device_id: [9; 32],
        parent_manifest_id: [0; 32],
        created_sec: at.0,
        created_nsec: at.1,
    }
}

pub fn prng(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..len).map(|_| rng.gen()).collect()
}

pub fn write_file(path: &Path, bytes: &[u8], exec: bool, mt: (i64, u32)) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(path).unwrap().permissions();
        perm.set_mode(if exec { 0o755 } else { 0o644 });
        std::fs::set_permissions(path, perm).unwrap();
    }
    #[cfg(not(unix))]
    let _ = exec;
    set_mtime(path, mt.0, mt.1);
}

pub fn set_mtime(path: &Path, sec: i64, nsec: u32) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::new(sec as u64, nsec)))
        .unwrap();
}
