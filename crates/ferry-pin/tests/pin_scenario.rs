//! T-015 acceptance: the scripted two-device pin scenario.
//!
//! Device A pins `src/**`, mutates three pinned files; device B mutates the
//! SAME three concurrently plus two disjoint ones. On A's next exchange
//! round B's offer arrives and the hold filter partitions the plan:
//!
//! - exactly 3 held decisions (the pinned src paths), ledgered under
//!   `.ferry/held/<peer>.jsonl`;
//! - A's bytes stay live in the tree for every pinned path;
//! - the disjoint B changes APPLY immediately;
//! - release replays the held set through the ordinary three-way engine
//!   with the pre-pin agreement as base: winner live, loser quarantined as
//!   `path.ferry-conflict.<loser>-<ts>`, conflicts.jsonl entries recorded;
//! - zero silent loss byte-verified across both trees;
//! - a second release is a no-op.
//!
//! Structure mirrors ferry-sync-engine's T-010 matrix harness: simulated
//! devices with isolated store/tree/state dirs, explicit mtimes so
//! three-way outcomes are deterministic, metadata-first exchange.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use ferry_pin::{
    hold_filter, plan_release, HeldLedger, HoldDecision, PinRecord, PinStore, PIN_FORMAT_VERSION,
};
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use ferry_store::store::Store;
use ferry_sync_engine::report::list_conflicts;
use ferry_sync_engine::{agree, execute, reconcile};
use rand::SeedableRng;

const DEV_A: [u8; 32] = [0xA1; 32];
const DEV_B: [u8; 32] = [0xB2; 32];
const FOLDER: [u8; 16] = [7; 16];
const NOW: (i64, u32) = (1_787_574_896, 0);

// ---------------------------------------------------------------------------
// device harness (mirrors the T-010 matrix Dev)
// ---------------------------------------------------------------------------

struct Dev {
    _dir: tempfile::TempDir,
    store: Store,
    tree: PathBuf,
    state: PathBuf,
    dev: [u8; 32],
    poly: u64,
    parent: [u8; 32],
    clock: i64,
}

impl Dev {
    fn new(tag: i64, dev: [u8; 32], poly: u64) -> Dev {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let store_root = root.join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = Store::create(&store_root, fmk(), Box::new(PassthroughCipher)).unwrap();
        let tree = root.join("tree");
        std::fs::create_dir_all(&tree).unwrap();
        Dev {
            _dir: dir,
            store,
            tree,
            state: root.join("state"),
            dev,
            poly,
            parent: [0; 32],
            clock: 1_787_000_000 + tag,
        }
    }

    fn snap(&mut self) -> SnapshotOutput {
        self.clock += 1;
        let idn = SnapshotIdentity {
            folder_id: FOLDER,
            device_id: self.dev,
            parent_manifest_id: self.parent,
            created_sec: self.clock,
            created_nsec: 0,
        };
        let out = snapshot_dir(&self.store, self.poly, &self.tree, &idn).unwrap();
        assert!(out.refused.is_empty());
        self.parent = out.manifest_id;
        out
    }
}

fn fmk() -> [u8; 32] {
    core::array::from_fn(|i| (i * 13 + 1) as u8)
}

fn poly(seed: u64) -> u64 {
    ferry_store::chunker::generate_polynomial(&mut rand::rngs::StdRng::seed_from_u64(seed))
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

/// Every file's full bytes under `root`, live and quarantined alike.
fn collect_bytes(root: &Path) -> HashSet<Vec<u8>> {
    let mut out = HashSet::new();
    fn walk(dir: &Path, out: &mut HashSet<Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.insert(std::fs::read(&p).unwrap());
            }
        }
    }
    walk(root, &mut out);
    out
}

/// Quarantined copy names anywhere under `root`.
fn quarantine_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    fn walk(dir: &Path, base: &Path, names: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, names);
            } else {
                let n = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                if n.contains(".ferry-conflict.") {
                    names.push(n);
                }
            }
        }
    }
    walk(root, root, &mut names);
    names.sort();
    names
}

/// Metadata-first exchange step: move the snapshot's manifest object and its
/// whole tree closure into the peer's store (what `REQ_META` serves).
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
        let node =
            ferry_store::manifest::parse_tree_node(&to.get(BlobKind::TreeNode, &id).unwrap())
                .unwrap();
        for e in node.entries {
            if let ferry_store::manifest::EntryPayload::Dir { child_tree_id } = e.payload {
                stack.push(child_tree_id);
            }
        }
    }
}

/// Data-phase step: pull every chunk the plan fetches from the peer's store
/// (what `REQ_DATA` serves). The fetch runs BEFORE the hold decision and stays
/// full across the split by design, so held versions' bytes are already in
/// A's store when release runs offline.
fn transfer_chunks(from: &Store, to: &Store, ids: &[(BlobId, u64)]) {
    for (id, _) in ids {
        if to.get(BlobKind::DataChunk, id).is_err() {
            let b = from
                .get(BlobKind::DataChunk, id)
                .expect("peer must hold a chunk it advertised");
            to.put_blob(BlobKind::DataChunk, &b).unwrap();
        }
    }
}

fn record_agreement(d: &Dev, peer: [u8; 32], manifest_id: BlobId) {
    agree::PeerState::new(&d.state)
        .record(&agree::AgreedRecord {
            peer_device_id: peer,
            manifest_id,
            agreed_sec: NOW.0,
            agreed_nsec: 0,
        })
        .unwrap();
}

fn load_agreed_manifest(d: &Dev, peer: [u8; 32]) -> Option<ferry_store::manifest::RootManifest> {
    let rec = agree::PeerState::new(&d.state).load(&peer).unwrap()?;
    let bytes = d.store.get(BlobKind::Manifest, &rec.manifest_id).ok()?;
    ferry_store::manifest::parse_manifest(&bytes).ok()
}

fn read_tree_file(root: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

// ---------------------------------------------------------------------------
// the scripted scenario
// ---------------------------------------------------------------------------

#[test]
fn pin_holds_concurrent_peer_edits_and_release_reconciles_per_adr0004() {
    let mut a = Dev::new(1, DEV_A, poly(42));
    let mut b = Dev::new(2, DEV_B, poly(42));

    // ---- common ancestor: identical trees on both devices ----------------
    for (rel, mt) in [
        ("src/a.rs", 1000),
        ("src/b.rs", 1000),
        ("src/c.rs", 1000),
        ("docs/d1.txt", 1000),
        ("docs/d2.txt", 1000),
    ] {
        write_file(&a.tree.join(rel), b"v0", (mt, 0));
        write_file(&b.tree.join(rel), b"v0", (mt, 0));
    }
    let sa_base = a.snap();
    let sb_base = b.snap();
    // Trees are identical, so each side records its own snapshot as the
    // last-agreed ancestor (same rule the exchange uses at equal roots).
    record_agreement(&a, DEV_B, sa_base.manifest_id);
    record_agreement(&b, DEV_A, sb_base.manifest_id);

    // ---- device A pins src/** -------------------------------------------
    let base_hex = hex(&sa_base.manifest_id);
    let mut base_agreements = BTreeMap::new();
    base_agreements.insert(hex(&DEV_B), base_hex);

    let pin_store = PinStore::new(&a.state);
    pin_store
        .start(&PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: hex(&DEV_A),
            pid: std::process::id(), // this test process: alive => holding
            started_sec: NOW.0,
            started_nsec: NOW.1,
            paths: vec!["src/**".into()],
            released: false,
            base_agreements: base_agreements.clone(),
        })
        .unwrap();
    assert!(pin_store.load().unwrap().expect("recorded").holding());

    // ---- concurrent mutation --------------------------------------------
    // A writes the three pinned files: two mtimes later than anything of
    // B's, one earlier, so release exercises BOTH winner directions.
    write_file(&a.tree.join("src/a.rs"), b"A-version-a", (3000, 0));
    write_file(&a.tree.join("src/b.rs"), b"A-version-b", (3000, 0));
    write_file(&a.tree.join("src/c.rs"), b"A-version-c", (2500, 0));

    // B rewrites the same three (losing a/b, winning c) plus two disjoint
    // docs files nobody pins.
    write_file(&b.tree.join("src/a.rs"), b"B-version-a", (2900, 0));
    write_file(&b.tree.join("src/b.rs"), b"B-version-b", (2950, 0));
    write_file(&b.tree.join("src/c.rs"), b"B-version-c", (3000, 0));
    write_file(&b.tree.join("docs/d1.txt"), b"B-docs-1", (2800, 0));
    write_file(&b.tree.join("docs/d2.txt"), b"B-docs-2", (2850, 0));

    let sa2 = a.snap(); // A's fresh scan at exchange time
    let sb = b.snap();

    // ---- exchange round on the pinned side -------------------------------
    transfer_meta(&b.store, &a.store, &sb);
    let remote = ferry_store::manifest::parse_manifest(
        &a.store.get(BlobKind::Manifest, &sb.manifest_id).unwrap(),
    )
    .unwrap();
    let plan = reconcile(ferry_sync_engine::reconcile::ReconcileInput {
        store: &a.store,
        local: &sa2.manifest,
        remote: &remote,
        base: load_agreed_manifest(&a, DEV_B).as_ref(),
    })
    .unwrap();
    transfer_chunks(&b.store, &a.store, &plan.fetch);

    let peer_hex = hex(&DEV_B);
    let manifest_hex = hex(&sb.manifest_id);
    let decision = hold_filter(
        &a.state,
        &a.store,
        &plan,
        &sa2.manifest,
        &peer_hex,
        &manifest_hex,
        NOW,
    )
    .unwrap();

    // ---- the hold ----------------------------------------------------------
    let HoldDecision::Hold(split) = decision else {
        panic!("an active scoped pin must hold the pinned-path decisions");
    };
    let held_paths: Vec<String> = split.held.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        held_paths,
        vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/c.rs".to_string()
        ],
        "exactly the three pinned paths hold"
    );
    for e in &split.held {
        assert_eq!(e.device_id, peer_hex);
        assert_eq!(e.remote_manifest_id, manifest_hex);
        assert!(!e.chunks.is_empty(), "held edits carry their blob refs");
    }

    // Disjoint B changes apply NOW; pinned paths keep A's bytes.
    let apply_stats = execute(&a.store, &a.tree, &split.apply, Some(&a.state), NOW).unwrap();
    assert_eq!(
        apply_stats.quarantined.len(),
        0,
        "disjoint changes cannot conflict"
    );
    assert_eq!(apply_stats.conflicts.len(), 0);
    assert_eq!(read_tree_file(&a.tree, "docs/d1.txt"), b"B-docs-1");
    assert_eq!(read_tree_file(&a.tree, "docs/d2.txt"), b"B-docs-2");
    assert_eq!(read_tree_file(&a.tree, "src/a.rs"), b"A-version-a");
    assert_eq!(read_tree_file(&a.tree, "src/b.rs"), b"A-version-b");
    assert_eq!(read_tree_file(&a.tree, "src/c.rs"), b"A-version-c");

    // Ledger persists the held set for status surfaces and release.
    let ledger = HeldLedger::new(&a.state);
    ledger.append(&peer_hex, &split.held).unwrap();
    assert_eq!(ledger.peers().unwrap(), vec![peer_hex.clone()]);
    let entries = ledger.load_peer(&peer_hex).unwrap();
    assert_eq!(
        ferry_pin::distinct_paths(&entries),
        held_paths,
        "status surface sees exactly the held set"
    );

    // ---- release -----------------------------------------------------------
    let sa3 = a.snap(); // rescan: the apply half changed the tree
    let plans = plan_release(&a.store, &sa3.manifest, &base_agreements, &ledger).unwrap();
    assert_eq!(plans.len(), 1, "one peer held changes");
    let rp = &plans[0];
    assert_eq!(rp.device_id, peer_hex);
    assert_eq!(rp.remote_manifest_id, manifest_hex);
    assert_eq!(rp.held_entries, 3);
    assert_eq!(rp.held_paths, held_paths);

    // Every distinct version that existed on either device before release;
    // nothing may vanish from the world afterwards.
    let mut truth = collect_bytes(&a.tree);
    truth.extend(collect_bytes(&b.tree));

    let stats = execute(&a.store, &a.tree, &rp.plan, Some(&a.state), NOW).unwrap();

    // Winners live per three-way (newer mtime wins), losers quarantined.
    assert_eq!(read_tree_file(&a.tree, "src/a.rs"), b"A-version-a");
    assert_eq!(read_tree_file(&a.tree, "src/b.rs"), b"A-version-b");
    assert_eq!(read_tree_file(&a.tree, "src/c.rs"), b"B-version-c");
    assert_eq!(
        stats.conflicts.len(),
        3,
        "every both-changed path reports explicitly"
    );

    // Quarantine accounting: loser copies exist with loser bytes.
    let qnames = quarantine_names(&a.tree);
    assert_eq!(qnames.len(), 3, "{qnames:?}");
    let qbytes: HashSet<Vec<u8>> = qnames
        .iter()
        .map(|n| std::fs::read(a.tree.join(n)).unwrap())
        .collect();
    for lost in [
        b"B-version-a".as_slice(),
        &b"B-version-b"[..],
        b"A-version-c",
    ] {
        assert!(qbytes.contains(lost), "{lost:?} must survive in quarantine");
    }
    for name in &qnames {
        assert!(
            name.contains(&hex(&DEV_A)[..8]) || name.contains(&hex(&DEV_B)[..8]),
            "quarantine names carry a device stamp: {name}"
        );
    }

    // Zero silent loss, byte-verified: everything that existed anywhere
    // before release survives somewhere on A afterwards (live or
    // quarantined). B's tree was never touched — prototype scope is manual
    // release on the pinning device; B converges later via ordinary sync.
    let after = collect_bytes(&a.tree);
    for v in &truth {
        assert!(
            after.contains(v),
            "lost version {:?}",
            String::from_utf8_lossy(v)
        );
    }

    // Structured conflict report: exactly the three releases, winners
    // matching the three-way decisions above.
    let log = list_conflicts(&a.state).unwrap();
    assert_eq!(log.len(), 3);
    let by_path: BTreeMap<&str, &ferry_sync_engine::ConflictEntry> =
        log.iter().map(|e| (e.path.as_str(), e)).collect();
    let ea = by_path.get("src/a.rs").unwrap();
    assert_eq!(ea.kind, "both_changed");
    assert_eq!(ea.winner.device, hex(&DEV_A));
    assert_eq!(ea.loser.device, hex(&DEV_B));
    assert_eq!(by_path.get("src/c.rs").unwrap().winner.device, hex(&DEV_B));
    for e in &log {
        assert!(e.quarantined_as.is_some());
    }

    // ---- finalize: clear ledgers, mark released, second release no-op -----
    assert!(ledger.clear_peer(&peer_hex).unwrap());
    assert!(pin_store.mark_released().unwrap());
    assert!(!pin_store.load().unwrap().unwrap().holding());

    let second = plan_release(&a.store, &sa3.manifest, &base_agreements, &ledger).unwrap();
    assert!(second.is_empty(), "second release must be a no-op");
    assert!(ledger.peers().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// stale pins surface loudly but never hold
// ---------------------------------------------------------------------------

#[test]
fn orphaned_writer_leaves_a_stale_pin_that_surfaces_but_does_not_hold() {
    let mut a = Dev::new(3, DEV_A, poly(42));

    // An orphaned daemon: a real process, killed, its pid stranded in the
    // pin record.
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleeper");
    let dead = {
        child.kill().expect("kill -9 equivalent");
        child.wait().expect("reap");
        child.id()
    };

    let pin_store = PinStore::new(&a.state);
    pin_store
        .start(&PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: hex(&DEV_A),
            pid: dead,
            started_sec: NOW.0,
            started_nsec: 0,
            paths: vec!["*".into()],
            released: false,
            base_agreements: BTreeMap::new(),
        })
        .unwrap();

    // Surfaced, not silently dropped: the record is readable and reports
    // WHY nothing is held (dead writer), and it was not quietly flipped to
    // released or deleted behind the user's back.
    let rec = pin_store.load().unwrap().expect("still on disk");
    assert!(!rec.holding(), "a dead writer cannot hold changes");
    assert!(!rec.released);

    // Any incoming plan passes through untouched while stale.
    let mut plan = ferry_sync_engine::ActionPlan::default();
    plan.materialize
        .push(ferry_sync_engine::plan::MaterializeOp {
            path: vec!["anything.txt".into()],
            base: None,
            result: None,
        });
    let local = a.snap().manifest;
    let decision = hold_filter(
        &a.state,
        &a.store,
        &plan,
        &local,
        &hex(&DEV_B),
        &"cc".repeat(32),
        NOW,
    )
    .unwrap();
    assert!(matches!(decision, HoldDecision::Pass));

    // Recovery path: a new start replaces the stale marker without error.
    pin_store
        .start(&PinRecord {
            pid: std::process::id(),
            ..rec.clone()
        })
        .unwrap();
    assert!(pin_store.load().unwrap().unwrap().holding());
}
