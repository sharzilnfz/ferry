//! Pack-granularity garbage collection behind a grace period.
//!
//! Reconciles with T-001/T-002 scope: the format spec defers pruning
//! ("Packs are immutable after rename. Pruning removes whole packs."), and
//! this module is exactly that pruning half, required by ticket T-002.
//!
//! Rules (documented contract of [`collect_garbage`]):
//! 1. NEVER delete a pack while ANY caller-designated live manifest can
//!    reach any blob inside it (whole-pack granularity: one live blob keeps
//!    the entire pack). Packs containing a polynomial record are always
//!    live -- losing the polynomial loses the folder's chunking.
//! 2. A pack whose EVERY blob is unreachable is garbage, but it is only
//!    deleted once it has been continuously unreferenced for longer than
//!    the grace period. First-seen-unreferenced timestamps live in a small
//!    local ledger (`.ferry/gc-state`) so the clock survives restarts;
//!    deleting the ledger only resets the clock, never correctness.
//! 3. Packs that fail name/footer verification are reported and skipped,
//!    never deleted (they may be evidence, and they cannot become worse).
//!
//! The grace period protects against concurrent writers: a writer stages
//! blobs, then commits a manifest referencing them. Anything written within
//! the last `grace` is treated as possibly-about-to-be-referenced. Within
//! one process, writers and GC share the store mutexes anyway; cross-process
//! racing GC against writers is accepted v0 residual risk (single-user CLI).

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
        // Tiny seal target so each blob tends to land in its own pack: this
        // makes whole-pack liveness deterministic for the assertions below.
        store.set_seal_target(512);
        (dir, store)
    }

    /// Commit a manifest object into the store and return its blob id.
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

        // Live world: M1 -> T1 -> chunks a,b (+ polynomial always live).
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

        // Orphans nobody references yet (simulating deleted snapshots).
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

        // Run 1 at t0: records unreferenced packs, deletes NOTHING.
        let r1 = collect_garbage(&store, &[m1], grace, t0).unwrap();
        assert!(r1.deleted.is_empty(), "nothing past grace yet");
        assert!(r1.recorded_unreferenced > 0, "orphans were recorded");

        // Run 2 halfway through grace: still nothing.
        let r2 = collect_garbage(&store, &[m1], grace, t0 + Duration::from_secs(5)).unwrap();
        assert!(r2.deleted.is_empty());

        // Run 3 past grace: fully-dead packs vanish, live data untouched.
        let r3 = collect_garbage(&store, &[m1], grace, t0 + Duration::from_secs(11)).unwrap();
        assert!(!r3.deleted.is_empty(), "orphan-only packs were collected");
        assert!(r3.deleted.len() < total_packs, "live packs must survive");

        // Every live blob is still readable.
        for id in [&a, &b, &t1, &m1, &poly_id] {
            store
                .get(BlobKind::DataChunk, id)
                .or_else(|_| store.get(BlobKind::TreeNode, id))
                .or_else(|_| store.get(BlobKind::Manifest, id))
                .or_else(|_| store.get(BlobKind::Polynomial, id))
                .unwrap_or_else(|e| panic!("live blob {} lost: {e}", crate::format::hex(id)));
        }

        // Fixpoint: after another full pass, NO pack remains whose contents
        // are entirely unreachable.
        collect_garbage(&store, &[m1], grace, t0 + Duration::from_secs(12)).unwrap();
        assert_no_dead_packs(&store, &[m1]);
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

        // Orphan content that will be "restored" mid-grace.
        let revived = chunk(&store, 60, 300);
        store.flush().unwrap();
        store.write_index_snapshot().unwrap();

        let t0 = SystemTime::now();
        let grace = Duration::from_secs(100);
        collect_garbage(&store, &[m1], grace, t0).unwrap(); // records orphans

        // Resurrection: a NEW manifest now references the old content.
        let _t2 = put_tree(
            &store,
            &TreeNode {
                entries: vec![file_entry("r", false, 0, 0, vec![(revived, 300)])],
            },
        );
        let m2 = put_manifest(&store, &manifest_for(_t2));
        store.flush().unwrap();
        store.write_index_snapshot().unwrap();

        // Long past the original recording time...
        let r = collect_garbage(&store, &[m1, m2], grace, t0 + Duration::from_secs(200)).unwrap();
        // ...but because the pack became REFERENCED again, it survived.
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

        // An extra pack whose name lies about its content.
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

    // --- helpers ---

    fn count_packs(store: &Store) -> usize {
        std::fs::read_dir(store.packs_dir()).unwrap().count()
    }

    /// After GC reaches a fixpoint, every remaining pack must contain at
    /// least one blob reachable from the live manifests (or a polynomial).
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

/// One ledger row: when a pack was FIRST seen unreferenced.
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
    /// Pack files inspected.
    pub scanned: usize,
    /// Packs deleted this run.
    pub deleted: Vec<BlobId>,
    /// Newly recorded unreferenced packs (not yet deletable).
    pub recorded_unreferenced: usize,
    /// Packs that failed verification; left alone on purpose.
    pub skipped_corrupt: Vec<String>,
}

/// Collect garbage packs per the module rules. `live_manifest_ids` names the
/// manifests the caller considers current; everything unreachable from them
/// is garbage. `now` is injected so callers (and tests) control time.
///
/// This function deletes files. It is deliberately conservative: see the
/// three rules at the top of this module.
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
                // The polynomial record keeps the folder chunkable; its
                // pack is always considered live.
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

/// Every (kind, id) pair reachable from the given live manifests: the
/// manifests themselves, their root tree nodes, all descendant tree nodes,
/// and all file chunks. Used as the liveness oracle by [`collect_garbage`].
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
            continue; // already walked
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

/// Ledger rows are `hex(pack_id) unix_nanos`; unreadable rows are ignored so
/// a damaged ledger degrades to "clock reset", never wrong deletions.
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
