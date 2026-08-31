use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const DEFAULT_STALE_AGE_SECS: u64 = 24 * 60 * 60;

pub fn sweep_store_temps(folder_root: &Path, max_age: Duration) -> std::io::Result<Vec<PathBuf>> {
    let cutoff = SystemTime::now()
        .checked_sub(max_age)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = Vec::new();

    let store_dir = folder_root.join(crate::store::STORE_DIR_NAME);

    let tmp_dir = store_dir.join("tmp");
    if let Ok(entries) = std::fs::read_dir(&tmp_dir) {
        for entry in entries.flatten() {
            if remove_if_stale(&entry.path(), cutoff)? {
                removed.push(entry.path());
            }
        }
    }

    type TempPredicate = fn(&str) -> bool;
    let sites: [(&str, TempPredicate); 3] = [
        ("", |n: &str| n.contains(".tmp")),
        ("peers", |n| n.starts_with(".tmp-")),
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
        Ok(t) if t < cutoff => match std::fs::remove_file(path) {
            Ok(()) => Ok(true),

            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        },
        Ok(_) => Ok(false),
        Err(_) => Ok(false),
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
