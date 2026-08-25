//! Crash-residue reclamation (T-20): one bounded, older-than sweep over
//! every Ferry-owned temp location under a synced folder.
//!
//! Every crash-recoverable write site stages through a temp file and
//! renames; a crash between write and rename orphans the temp. This module
//! reclaims such residue in one pass so callers can run it once at startup
//! (documented startup sequence: `SyncEngine::new` sweeps before any
//! session thread starts). The sweep is BOUNDED: only files whose mtime is
//! older than `max_age` are removed, so a concurrent writer's in-flight
//! temp is never touched and an interrupted large transfer keeps its
//! resumable bytes briefly (same tradeoff as Syncthing, mirrored by
//! `ferry_materialize::sweep_stale_temps` for tree-side temps).
//!
//! Sites swept here (each named with its producer):
//!
//! | Location                          | Producer                              |
//! |-----------------------------------|---------------------------------------|
//! | `.ferry/tmp/*`                    | pack sealing (`pack.rs`), index and   |
//! |                                   | gc-state staging                      |
//! |                                   | (`index::write_named_atomically`)     |
//! | `.ferry/<sidecar>.tmp*`           | settings writer (`folder.rs`), pin    |
//! |                                   | state writer (`pin.rs`)               |
//! | `.ferry/peers/.tmp-*`             | peer-record writer (`PeerLedger`)     |
//! | `.ferry/agreement/.tmp-*`         | agreement writer (`AgreementLedger`)  |
//!
//! Tree-side materialize temps (`.ferry.<name>.tmp`, `~ferry~<name>.tmp`)
//! live INSIDE the synced tree, not under `.ferry/`; they have their own
//! reserved-name grammar and sweeper in ferry-materialize and are swept by
//! the same startup hook. Quarantined conflict copies are ordinary tree
//! files owned by ADR-0004 semantics — never swept here.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long orphaned temp files are kept before sweeping. Parity constant
/// with `ferry_materialize::DEFAULT_STALE_TEMP_AGE_SECS`; kept separate so
/// each crate owns its own contract.
pub const DEFAULT_STALE_AGE_SECS: u64 = 24 * 60 * 60;

/// Remove every stale temp file under `<folder_root>/.ferry/` per the module
/// rules. Returns the removed paths, sorted. Missing directories are fine
/// (fresh folders); missing files mid-sweep are races, also fine.
pub fn sweep_store_temps(folder_root: &Path, max_age: Duration) -> std::io::Result<Vec<PathBuf>> {
    let cutoff = SystemTime::now()
        .checked_sub(max_age)
        // A max_age larger than the clock spanned since the epoch means
        // "everything"; clamp to zero instead of failing.
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = Vec::new();

    let store_dir = folder_root.join(crate::store::STORE_DIR_NAME);

    // 1. The flat staging dir: EVERYTHING in it is producer residue once old.
    let tmp_dir = store_dir.join("tmp");
    if let Ok(entries) = std::fs::read_dir(&tmp_dir) {
        for entry in entries.flatten() {
            if remove_if_stale(&entry.path(), cutoff)? {
                removed.push(entry.path());
            }
        }
    }

    // 2..4. Named temp files beside their live sidecars.
    type TempPredicate = fn(&str) -> bool;
    let sites: [(&str, TempPredicate); 3] = [
        // Top-level sidecar temps: `settings.json.tmp`, `pin-state.json.tmp.<pid>.<seq>.<nsec>`.
        // Live sidecars never contain ".tmp", so containment is unambiguous here.
        ("", |n: &str| n.contains(".tmp")),
        // Peer-record temps from PeerLedger::record_peer.
        ("peers", |n| n.starts_with(".tmp-")),
        // Agreement temps from AgreementLedger::record.
        ("agreement", |n| n.starts_with(".tmp-")),
    ];
    for (subdir, is_temp) in sites {
        let dir = if subdir.is_empty() {
            store_dir.clone()
        } else {
            store_dir.join(subdir)
        };
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !is_temp(&name) {
                    continue;
                }
                if remove_if_stale(&entry.path(), cutoff)? {
                    removed.push(entry.path());
                }
            }
        }
    }

    removed.sort();
    Ok(removed)
}

fn remove_if_stale(path: &Path, cutoff: SystemTime) -> std::io::Result<bool> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    match meta.modified() {
        Ok(t) if t < cutoff => {
            match std::fs::remove_file(path) {
                Ok(()) => Ok(true),
                // Lost a race with the producer's rename; nothing to do.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(e),
            }
        }
        Ok(_) => Ok(false),
        Err(_) => Ok(false), // no mtime on this host: keep, never guess
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::hex;
    use std::time::Duration;

    fn set_old_mtime(path: &Path) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_times(std::fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
            .unwrap();
    }

    fn assert_swept(removed: &[PathBuf], p: &Path) {
        assert!(
            removed.iter().any(|r| r == p),
            "{p:?} should have been swept, got {removed:?}"
        );
        assert!(!p.exists());
    }

    #[test]
    fn stale_temps_at_every_documented_site_are_removed_fresh_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let sd = root.join(".ferry");
        for d in ["tmp", "peers", "agreement"] {
            std::fs::create_dir_all(sd.join(d)).unwrap();
        }

        // Stale residue at every site, with realistic producer names.
        let stale = [
            sd.join("tmp").join(format!("pack-{}.tmp", hex(&[7u8; 16]))),
            sd.join("tmp").join("gc-state"),
            sd.join("settings.json.tmp"),
            sd.join(format!("pin-state.json.tmp.{}.0.42", std::process::id())),
            sd.join("peers").join(".tmp-aabb-ccdd"),
            sd.join("agreement")
                .join(format!(".tmp-{}-{}", hex(&[1u8; 16]), hex(&[2u8; 32]))),
        ];
        let fresh = [
            sd.join("tmp").join(format!("pack-{}.tmp", hex(&[9u8; 16]))),
            sd.join("peers")
                .join(format!("{}-{}.peer", hex(&[1u8; 16]), hex(&[2u8; 32]))),
            sd.join("agreement")
                .join(format!("{}-{}.agree", hex(&[1u8; 16]), hex(&[2u8; 32]))),
        ];
        for p in stale.iter().chain(fresh.iter()) {
            std::fs::write(p, b"residue").unwrap();
        }
        for p in &stale {
            set_old_mtime(p);
        }

        let removed = sweep_store_temps(root, Duration::from_hours(1)).unwrap();
        for p in &stale {
            assert_swept(&removed, p);
        }
        for p in &fresh {
            assert!(p.exists(), "live/fresh file {p:?} must survive");
        }

        // Idempotent: second sweep removes nothing more.
        let again = sweep_store_temps(root, Duration::from_hours(1)).unwrap();
        assert!(again.is_empty(), "{again:?}");
    }

    #[test]
    fn missing_store_dir_is_a_clean_noop() {
        let dir = tempfile::tempdir().unwrap();
        let removed = sweep_store_temps(dir.path(), Duration::from_hours(1)).unwrap();
        assert!(removed.is_empty());
    }

    #[test]
    fn max_age_zero_sweeps_every_temp_regardless_of_age() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join(".ferry/tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("pack-fresh.tmp");
        std::fs::write(&p, b"just written").unwrap();
        let removed = sweep_store_temps(dir.path(), Duration::ZERO).unwrap();
        assert_eq!(removed, vec![p.clone()]);
        assert!(!p.exists());
    }
}
