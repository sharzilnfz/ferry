use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use ferry_store::agreement::{AgreedRecord, AgreementLedger};
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use ferry_store::store::Store;
use ferry_sync_engine::pin::{release_peer, HeldLedger, PinRecord, PinStore, PIN_FORMAT_VERSION};
use ferry_sync_engine::report::list_conflicts;
use ferry_sync_engine::{ConvergenceEngine, ConvergenceError};
use rand::SeedableRng;

const DEV_A: [u8; 32] = [0xA1; 32];
const DEV_B: [u8; 32] = [0xB2; 32];
const FOLDER: [u8; 16] = [7; 16];
const NOW: (i64, u32) = (1_787_574_896, 0);

struct Dev {
    _dir: tempfile::TempDir,
    store: Store,
    tree: PathBuf,
    state: PathBuf,
    dev: [u8; 32],
    poly: ferry_store::chunker::ValidatedPoly,
    parent: [u8; 32],
    clock: i64,
}

impl Dev {
    fn new(tag: i64, dev: [u8; 32], poly: ferry_store::chunker::ValidatedPoly) -> Dev {
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
    AgreementLedger::new(&d.state)
        .record(
            &FOLDER,
            &AgreedRecord {
                peer_device_id: peer,
                manifest_id,
                agreed_sec: NOW.0,
                agreed_nsec: 0,
            },
        )
        .unwrap();
}

fn load_agreed_manifest(d: &Dev, peer: [u8; 32]) -> Option<ferry_store::manifest::RootManifest> {
    let rec = AgreementLedger::new(&d.state)
        .get(&FOLDER, &peer)
        .unwrap()?;
    let bytes = d.store.get(BlobKind::Manifest, &rec.manifest_id).ok()?;
    ferry_store::manifest::parse_manifest(&bytes).ok()
}

fn read_tree_file(root: &Path, rel: &str) -> Vec<u8> {
    std::fs::read(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

fn unhex_32(s: &str) -> Option<[u8; 32]> {
    ferry_store::format::unhex::<32>(s)
}

struct PeerFetch<'x> {
    from: &'x ferry_store::store::Store,
    to: &'x ferry_store::store::Store,
}

impl ferry_sync_engine::BlobFetch for PeerFetch<'_> {
    fn fetch(&mut self, want: &[(BlobId, u64)]) -> Result<(), ConvergenceError> {
        transfer_chunks(self.from, self.to, want);
        Ok(())
    }
}

#[test]
fn pin_holds_concurrent_peer_edits_and_release_reconciles_per_adr0004() {
    let mut a = Dev::new(1, DEV_A, poly(42));
    let mut b = Dev::new(2, DEV_B, poly(42));

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
    transfer_meta(&a.store, &b.store, &sa_base);
    b.parent = sa_base.manifest_id;

    record_agreement(&a, DEV_B, sa_base.manifest_id);
    record_agreement(&b, DEV_A, sa_base.manifest_id);

    let base_hex = hex(&sa_base.manifest_id);
    let mut base_agreements = BTreeMap::new();
    base_agreements.insert(hex(&DEV_B), base_hex);

    let pin_store = PinStore::new(&a.state);
    pin_store
        .start(&PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: hex(&DEV_A),
            pid: std::process::id(),
            started_sec: NOW.0,
            started_nsec: NOW.1,
            expires_sec: None,
            paths: vec!["src/**".into()],
            released: false,
            base_agreements: base_agreements.clone(),
            proc_start_token: None,
        })
        .unwrap();
    assert!(pin_store.load().unwrap().expect("recorded").holding());

    write_file(&a.tree.join("src/a.rs"), b"A-version-a", (3000, 0));
    write_file(&a.tree.join("src/b.rs"), b"A-version-b", (3000, 0));
    write_file(&a.tree.join("src/c.rs"), b"A-version-c", (2500, 0));

    write_file(&b.tree.join("src/a.rs"), b"B-version-a", (2900, 0));
    write_file(&b.tree.join("src/b.rs"), b"B-version-b", (2950, 0));
    write_file(&b.tree.join("src/c.rs"), b"B-version-c", (3000, 0));
    write_file(&b.tree.join("docs/d1.txt"), b"B-docs-1", (2800, 0));
    write_file(&b.tree.join("docs/d2.txt"), b"B-docs-2", (2850, 0));

    let sa2 = a.snap();
    let sb = b.snap();

    transfer_meta(&b.store, &a.store, &sb);
    let remote = ferry_store::manifest::parse_manifest(
        &a.store.get(BlobKind::Manifest, &sb.manifest_id).unwrap(),
    )
    .unwrap();
    let peer_hex = hex(&DEV_B);
    let manifest_hex = hex(&sb.manifest_id);
    let mut fetch = PeerFetch {
        from: &b.store,
        to: &a.store,
    };

    let result = ConvergenceEngine::new(&a.store, &a.tree)
        .state_dir(&a.state)
        .at(NOW)
        .fetch_with(&mut fetch)
        .converge(
            &sa2.manifest,
            &remote,
            load_agreed_manifest(&a, DEV_B).as_ref(),
        )
        .unwrap();

    let held_paths: Vec<String> = result.held.iter().map(|h| h.path.clone()).collect();
    assert_eq!(
        held_paths,
        vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "src/c.rs".to_string()
        ],
        "exactly the three pinned paths hold"
    );
    for h in &result.held {
        assert!(!h.chunks.is_empty(), "held edits carry their blob refs");
    }

    assert_eq!(
        result.quarantined.len(),
        0,
        "disjoint changes cannot conflict"
    );
    assert_eq!(result.conflicts.len(), 0);
    assert_eq!(read_tree_file(&a.tree, "docs/d1.txt"), b"B-docs-1");
    assert_eq!(read_tree_file(&a.tree, "docs/d2.txt"), b"B-docs-2");
    assert_eq!(read_tree_file(&a.tree, "src/a.rs"), b"A-version-a");
    assert_eq!(read_tree_file(&a.tree, "src/b.rs"), b"A-version-b");
    assert_eq!(read_tree_file(&a.tree, "src/c.rs"), b"A-version-c");

    let ledger = HeldLedger::new(&a.state);
    assert_eq!(ledger.peers().unwrap(), vec![peer_hex.clone()]);
    let entries = ledger.load_peer(&peer_hex).unwrap();
    assert_eq!(entries.len(), 3);
    for e in &entries {
        assert_eq!(e.device_id, peer_hex);
        assert_eq!(e.remote_manifest_id, manifest_hex);
    }
    assert_eq!(
        ferry_sync_engine::distinct_paths(&entries),
        held_paths,
        "status surface sees exactly the held set"
    );

    let sa3 = a.snap();
    let base_hex = base_agreements.get(&peer_hex).unwrap();
    let base_bytes = a
        .store
        .get(BlobKind::Manifest, &unhex_32(base_hex).unwrap())
        .unwrap();
    let base = ferry_store::manifest::parse_manifest(&base_bytes).unwrap();
    let rp = release_peer(
        &a.store,
        &a.tree,
        &a.state,
        &sa3.manifest,
        &peer_hex,
        Some(&base),
        NOW,
    )
    .unwrap();
    assert_eq!(rp.device_id, peer_hex);
    assert_eq!(rp.remote_manifest_id, manifest_hex);
    assert_eq!(rp.held_entries, 3);
    assert_eq!(rp.held_paths, held_paths);

    let mut truth = collect_bytes(&a.tree);
    truth.extend(collect_bytes(&b.tree));

    let stats = &rp.result;

    assert_eq!(read_tree_file(&a.tree, "src/a.rs"), b"A-version-a");
    assert_eq!(read_tree_file(&a.tree, "src/b.rs"), b"A-version-b");
    assert_eq!(read_tree_file(&a.tree, "src/c.rs"), b"B-version-c");
    assert_eq!(
        stats.conflicts.len(),
        3,
        "every both-changed path reports explicitly"
    );

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

    let after = collect_bytes(&a.tree);
    for v in &truth {
        assert!(
            after.contains(v),
            "lost version {:?}",
            String::from_utf8_lossy(v)
        );
    }

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

    assert!(ledger.clear_peer(&peer_hex).unwrap());
    assert!(pin_store.mark_released().unwrap());
    assert!(!pin_store.load().unwrap().unwrap().holding());

    let second = release_peer(
        &a.store,
        &a.tree,
        &a.state,
        &sa3.manifest,
        &peer_hex,
        Some(&base),
        NOW,
    )
    .unwrap();
    assert_eq!(second.held_entries, 0, "second release must be a no-op");
    assert!(second.result.is_noop());
    assert!(ledger.peers().unwrap().is_empty());
}

#[test]
fn orphaned_writer_leaves_a_stale_pin_that_surfaces_but_does_not_hold() {
    let mut a = Dev::new(3, DEV_A, poly(42));

    let mut child = ferry_platform::spawn_sleeper(30).expect("spawn sleeper");
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
            expires_sec: None,
            paths: vec!["*".into()],
            released: false,
            base_agreements: BTreeMap::new(),
            proc_start_token: None,
        })
        .unwrap();

    let rec = pin_store.load().unwrap().expect("still on disk");
    assert!(!rec.holding(), "a dead writer cannot hold changes");
    assert!(!rec.released);

    let local = a.snap().manifest;
    assert!(!pin_store.load().unwrap().unwrap().holding());
    let _ = local;

    pin_store
        .start(&PinRecord {
            pid: std::process::id(),
            ..rec.clone()
        })
        .unwrap();
    assert!(pin_store.load().unwrap().unwrap().holding());
}
