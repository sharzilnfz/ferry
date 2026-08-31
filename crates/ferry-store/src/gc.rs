#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{PassthroughCipher, KEY_LEN};
    #[allow(unused_imports)]
    use crate::format::PackId;
    use crate::manifest::{
        file_entry, serialize_manifest, serialize_tree_node, RootManifest, TreeNode,
    };
    use crate::store::Store;
    use std::time::{Duration, SystemTime};

    fn fmk() -> [u8; KEY_LEN] {
        core::array::from_fn(|i| i as u8)
    }

    fn fresh() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::create(dir.path(), fmk(), Box::new(PassthroughCipher)).unwrap();

        store.set_seal_target(512);
        (dir, store)
    }

    fn put_manifest(store: &Store, m: &RootManifest) -> BlobId {
        let bytes = serialize_manifest(m);
        store.put_meta(BlobKind::Manifest, &bytes).unwrap()
    }

    fn put_tree(store: &Store, t: &TreeNode) -> BlobId {
        let bytes = serialize_tree_node(t);
        store.put_meta(BlobKind::TreeNode, &bytes).unwrap()
    }

    fn manifest_for(root_tree: BlobId) -> RootManifest {
        RootManifest {
            folder_id: [1; 16],
            device_id: [2; 32],
            created_sec: 100,
            created_nsec: 0,
            root_tree_id: root_tree,
            parent_manifest_id: [0; 32],
        }
    }

    fn chunk(store: &Store, seed: u8, len: usize) -> BlobId {
        let bytes: Vec<u8> = (0..len).map(|i| seed.wrapping_add(i as u8)).collect();
        store.put_data(&bytes).unwrap()
    }

    #[test]
    fn gc_deletes_only_fully_unreferenced_packs_after_grace() {
        let (_dir, store) = fresh();

        let a = chunk(&store, 10, 300);
        let b = chunk(&store, 20, 300);
        let poly_id = store.put_polynomial(0x1234).unwrap();
        let t1 = put_tree(
            &store,
            &TreeNode {
                entries: vec![
                    file_entry("a", false, 0, 0, vec![(a, 300)]),
                    file_entry("b", false, 0, 0, vec![(b, 300)]),
                ],
            },
        );
        let m1 = put_manifest(&store, &manifest_for(t1));

        let _c = chunk(&store, 30, 300);
        let d = chunk(&store, 40, 300);
        let _t2 = put_tree(
            &store,
            &TreeNode {
                entries: vec![file_entry("d", false, 0, 0, vec![(d, 300)])],
            },
        );

        store.flush().unwrap();
        store.write_index_snapshot().unwrap();
        let total_packs = count_packs(&store);

        let t0 = SystemTime::now();
        let grace = Duration::from_secs(10);

        let r1 = collect_garbage(&store, &[m1], grace, t0).unwrap();
        assert!(r1.deleted.is_empty(), "nothing past grace yet");
        assert!(r1.recorded_unreferenced > 0, "orphans were recorded");

        let r2 = collect_garbage(&store, &[m1], grace, t0 + Duration::from_secs(5)).unwrap();
        assert!(r2.deleted.is_empty());

        let r3 = collect_garbage(&store, &[m1], grace, t0 + Duration::from_secs(11)).unwrap();
        assert!(!r3.deleted.is_empty(), "orphan-only packs were collected");
        assert!(r3.deleted.len() < total_packs, "live packs must survive");

        for id in [&a, &b, &t1, &m1, &poly_id] {
            store
                .get(BlobKind::DataChunk, id)
                .or_else(|_| store.get(BlobKind::TreeNode, id))
                .or_else(|_| store.get(BlobKind::Manifest, id))
                .or_else(|_| store.get(BlobKind::Polynomial, id))
                .unwrap_or_else(|e| panic!("live blob {} lost: {e}", crate::format::hex(id)));
        }

        collect_garbage(&store, &[m1], grace, t0 + Duration::from_secs(12)).unwrap();
        assert_no_dead_packs(&store, &[m1]);
    }

    #[test]
    fn reachability_report_lists_superseded_packs_and_never_live_ones() {
        let (_dir, store) = fresh();

        let a = chunk(&store, 10, 300);
        let b = chunk(&store, 20, 300);
        let poly_id = store.put_polynomial(0x1234).unwrap();
        let t1 = put_tree(
            &store,
            &TreeNode {
                entries: vec![
                    file_entry("a", false, 0, 0, vec![(a, 300)]),
                    file_entry("b", false, 0, 0, vec![(b, 300)]),
                ],
            },
        );
        let m1 = put_manifest(&store, &manifest_for(t1));

        let d = chunk(&store, 40, 300);
        let _t2 = put_tree(
            &store,
            &TreeNode {
                entries: vec![file_entry("d", false, 0, 0, vec![(d, 300)])],
            },
        );

        store.flush().unwrap();
        store.write_index_snapshot().unwrap();

        let r = reachability_report(&store, &[m1]).unwrap();
        assert!(r.scanned_packs > 0);
        assert!(r.live_packs > 0, "the live pack set is nonempty");
        assert!(r.skipped_corrupt.is_empty());
        assert_eq!(r.garbage_packs.len(), 1, "exactly one dead pack: {r:?}");
        assert!(r.reclaimable_bytes > 0);

        let garbage_contents: HashSet<_> = r.garbage_packs.iter().map(|(id, _)| *id).collect();
        for path in std::fs::read_dir(store.packs_dir()).unwrap().flatten() {
            let stem = path.file_name().to_string_lossy().to_string();
            let Some(claimed) = crate::format::unhex::<32>(stem.trim_end_matches(".pack")) else {
                continue;
            };
            if !garbage_contents.contains(&claimed) {
                continue;
            }
            let (_, entries) = store.pack_blob_list(&claimed).unwrap();
            for e in entries {
                let live_ids = [a, b, t1, m1, poly_id];
                assert!(
                    !live_ids.contains(&e.id),
                    "live blob {} found inside a garbage pack",
                    crate::format::hex(&e.id)
                );
            }
        }

        let m2 = put_manifest(&store, &manifest_for(_t2));
        let r_all_live = reachability_report(&store, &[m1, m2]).unwrap();
        assert!(r_all_live.garbage_packs.is_empty(), "{r_all_live:?}");
    }

    #[test]
    fn resurrected_content_is_protected_by_the_ledger() {
        let (_dir, store) = fresh();

        let live_a = chunk(&store, 50, 300);
        let t1 = put_tree(
            &store,
            &TreeNode {
                entries: vec![file_entry("a", false, 0, 0, vec![(live_a, 300)])],
            },
        );
        let m1 = put_manifest(&store, &manifest_for(t1));

        let revived = chunk(&store, 60, 300);
        store.flush().unwrap();
        store.write_index_snapshot().unwrap();

        let t0 = SystemTime::now();
        let grace = Duration::from_secs(100);
        collect_garbage(&store, &[m1], grace, t0).unwrap();

        let _t2 = put_tree(
            &store,
            &TreeNode {
                entries: vec![file_entry("r", false, 0, 0, vec![(revived, 300)])],
            },
        );
        let m2 = put_manifest(&store, &manifest_for(_t2));
        store.flush().unwrap();
        store.write_index_snapshot().unwrap();

        let r = collect_garbage(&store, &[m1, m2], grace, t0 + Duration::from_secs(200)).unwrap();

        store.get(BlobKind::DataChunk, &revived).unwrap();
        let _ = r;
    }

    #[test]
    fn corrupt_pack_is_reported_and_never_deleted_by_gc() {
        let (_dir, store) = fresh();
        let a = chunk(&store, 70, 200);
        let t = put_tree(
            &store,
            &TreeNode {
                entries: vec![file_entry("a", false, 0, 0, vec![(a, 200)])],
            },
        );
        let m = put_manifest(&store, &manifest_for(t));

        let liar = store.packs_dir().join(format!("{}.pack", "f".repeat(64)));
        std::fs::write(&liar, b"garbage pretending to be a pack").unwrap();

        let far_future = SystemTime::now() + Duration::from_secs(10_000);
        let r = collect_garbage(&store, &[m], Duration::ZERO, far_future).unwrap();
        assert_eq!(r.skipped_corrupt.len(), 1);
        assert!(liar.exists(), "GC must not delete unverifiable packs");
    }

    #[test]
    fn empty_store_gc_is_a_noop() {
        let (_dir, store) = fresh();
        store.flush().unwrap();
        let r = collect_garbage(&store, &[], Duration::ZERO, SystemTime::now()).unwrap();
        assert!(r.deleted.is_empty());
        assert_eq!(r.scanned, 0);
    }

    fn count_packs(store: &Store) -> usize {
        std::fs::read_dir(store.packs_dir()).unwrap().count()
    }

    fn assert_no_dead_packs(store: &Store, live: &[BlobId]) {
        let reachable = collect_referenced(store, live).unwrap();
        for entry in std::fs::read_dir(store.packs_dir()).unwrap().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let claimed = crate::format::unhex::<32>(name.trim_end_matches(".pack")).unwrap();
            let bytes = std::fs::read(entry.path()).unwrap();
            let (_, entries) =
                crate::pack::read_footer(&bytes, &claimed, &fmk(), &PassthroughCipher)
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
            let alive = entries.iter().any(|e| match e.kind {
                BlobKind::Polynomial => true,
                _ => reachable.contains(&(e.kind, e.id)),
            });
            assert!(alive, "pack {name} holds only dead blobs after fixpoint");
        }
    }
}

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::format::{hex, BlobId, BlobKind};
use crate::manifest::{parse_manifest, parse_tree_node};
use crate::store::{Store, StoreError};

const LEDGER_FILE: &str = "gc-state";

#[derive(Debug, Error)]
pub enum GcError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("manifest decode failed: {0}")]
    Manifest(#[from] crate::manifest::ManifestError),
    #[error("{0}")]
    Index(#[from] crate::index::IndexError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("gc-state ledger has no parent directory")]
    NoLedgerParent,
}

#[derive(Debug, Default)]
pub struct GcReport {
    pub scanned: usize,

    pub deleted: Vec<BlobId>,

    pub recorded_unreferenced: usize,

    pub skipped_corrupt: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ReachabilityReport {
    pub scanned_packs: usize,

    pub live_packs: usize,

    pub garbage_packs: Vec<(BlobId, u64)>,

    pub reclaimable_bytes: u64,

    pub skipped_corrupt: Vec<String>,
}

pub fn reachability_report(
    store: &Store,
    live_manifest_ids: &[BlobId],
) -> Result<ReachabilityReport, GcError> {
    let reachable = collect_referenced(store, live_manifest_ids)?;

    let mut packs: Vec<PathBuf> = std::fs::read_dir(store.packs_dir())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    packs.sort();

    let mut report = ReachabilityReport::default();
    for path in packs {
        report.scanned_packs += 1;
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let Some(claimed) = crate::format::unhex::<32>(&stem) else {
            report.skipped_corrupt.push(stem);
            continue;
        };
        let pack_is_live = match store.pack_blob_list(&claimed) {
            Ok((_, entries)) => entries.iter().any(|e| match e.kind {
                BlobKind::Polynomial => true,
                _ => reachable.contains(&(e.kind, e.id)),
            }),
            Err(_) => {
                report.skipped_corrupt.push(stem);
                continue;
            }
        };
        if pack_is_live {
            report.live_packs += 1;
        } else {
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            report.garbage_packs.push((claimed, size));
            report.reclaimable_bytes += size;
        }
    }
    Ok(report)
}

pub fn collect_garbage(
    store: &Store,
    live_manifest_ids: &[BlobId],
    grace: Duration,
    now: SystemTime,
) -> Result<GcReport, GcError> {
    let mut report = GcReport::default();
    let reachable = collect_referenced(store, live_manifest_ids)?;

    let mut ledger = load_ledger(&ledger_path(store))?;

    let mut packs: Vec<PathBuf> = std::fs::read_dir(store.packs_dir())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "pack"))
        .collect();
    packs.sort();

    let mut ledger_dirty = false;
    for path in packs {
        report.scanned += 1;
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        let Some(claimed) = crate::format::unhex::<32>(&stem) else {
            report.skipped_corrupt.push(stem);
            continue;
        };
        let bytes = std::fs::read(&path)?;
        let _ = &bytes;
        let pack_is_live = match store.pack_blob_list(&claimed) {
            Ok((_, entries)) => entries.iter().any(|e| match e.kind {
                BlobKind::Polynomial => true,
                _ => reachable.contains(&(e.kind, e.id)),
            }),
            Err(_) => {
                report.skipped_corrupt.push(stem);
                continue;
            }
        };

        if pack_is_live {
            if ledger.remove(&claimed).is_some() {
                ledger_dirty = true;
            }
            continue;
        }

        let first_unref = match ledger.get(&claimed) {
            Some(t) => *t,
            None => {
                ledger.insert(claimed, now);
                ledger_dirty = true;
                report.recorded_unreferenced += 1;
                now
            }
        };
        let elapsed = now.duration_since(first_unref).unwrap_or(Duration::ZERO);
        if elapsed >= grace {
            std::fs::remove_file(&path)?;
            store.invalidate_pack(&claimed);
            ledger.remove(&claimed);
            ledger_dirty = true;
            report.deleted.push(claimed);
        }
    }

    if ledger_dirty || !report.deleted.is_empty() {
        save_ledger(&ledger_path(store), &ledger)?;
    }
    Ok(report)
}

pub fn collect_referenced(
    store: &Store,
    live_manifest_ids: &[BlobId],
) -> Result<HashSet<(BlobKind, BlobId)>, GcError> {
    let mut set = HashSet::new();
    let mut tree_stack = Vec::new();

    for m_id in live_manifest_ids {
        set.insert((BlobKind::Manifest, *m_id));
        let bytes = store.get(BlobKind::Manifest, m_id)?;
        let manifest = parse_manifest(&bytes)?;
        tree_stack.push(manifest.root_tree_id);
    }

    while let Some(tree_id) = tree_stack.pop() {
        if !set.insert((BlobKind::TreeNode, tree_id)) {
            continue;
        }
        let bytes = store.get(BlobKind::TreeNode, &tree_id)?;
        let node = parse_tree_node(&bytes)?;
        for (kind, id) in node.referenced_blobs() {
            match kind {
                BlobKind::DataChunk => {
                    set.insert((BlobKind::DataChunk, id));
                }
                BlobKind::TreeNode => tree_stack.push(id),
                other => {
                    set.insert((other, id));
                }
            }
        }
    }
    Ok(set)
}

fn ledger_path(store: &Store) -> PathBuf {
    store.store_dir().join(LEDGER_FILE)
}

type Ledger = std::collections::HashMap<BlobId, SystemTime>;

fn load_ledger(path: &std::path::Path) -> Result<Ledger, GcError> {
    let mut map = Ledger::new();
    if !path.exists() {
        return Ok(map);
    }
    let text = std::fs::read_to_string(path)?;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(id_str), Some(ts_str)) = (parts.next(), parts.next()) else {
            continue;
        };
        let (Some(id), Some(nanos)) = (
            crate::format::unhex::<32>(id_str),
            ts_str.parse::<u128>().ok(),
        ) else {
            continue;
        };
        map.insert(
            id,
            SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos as u64),
        );
    }
    Ok(map)
}

fn save_ledger(path: &std::path::Path, ledger: &Ledger) -> Result<(), GcError> {
    let mut text = String::new();
    let mut rows: Vec<_> = ledger.iter().collect();
    rows.sort_by_key(|(_, t)| {
        t.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
    });
    for (id, t) in rows {
        let nanos = t
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        let _ = writeln!(text, "{} {}", hex(id), nanos);
    }
    let parent = path.parent().ok_or(GcError::NoLedgerParent)?;
    crate::index::write_named_atomically(
        &parent.join("tmp"),
        parent,
        LEDGER_FILE,
        text.as_bytes(),
    )?;
    Ok(())
}
