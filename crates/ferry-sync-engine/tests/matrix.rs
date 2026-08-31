use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use ferry_store::store::Store;
use ferry_sync_engine::report::list_conflicts;
use ferry_sync_engine::{ConvergenceEngine, ConvergenceError, ConvergenceResult};
use rand::SeedableRng;

const DEV_A: [u8; 32] = [0xA1; 32];
const DEV_B: [u8; 32] = [0xB2; 32];
const FOLDER: [u8; 16] = [7; 16];
const NOW: (i64, u32) = (1_787_574_896, 0);

#[derive(Clone, Copy, Debug)]
enum Scenario {
    BothChange,
    DeleteVsEdit,
    AddVsAddSame,
    AddVsAddDiff,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Direction {
    AWins,
    BWins,
    Tie,
}

#[derive(Clone, Copy, Debug)]
enum Order {
    AFirst,
    BFirst,
    Simultaneous,
}

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

fn load_base_object(d: &Dev, id: Option<BlobId>) -> Option<ferry_store::manifest::RootManifest> {
    let bytes = d.store.get(BlobKind::Manifest, &id?).ok()?;
    ferry_store::manifest::parse_manifest(&bytes).ok()
}

struct PeerFetch<'x> {
    from: &'x Store,
    to: &'x Store,
}

impl ferry_sync_engine::BlobFetch for PeerFetch<'_> {
    fn fetch(&mut self, want: &[(BlobId, u64)]) -> Result<(), ConvergenceError> {
        for (id, _) in want {
            if self.to.get(BlobKind::DataChunk, id).is_err() {
                let b = self
                    .from
                    .get(BlobKind::DataChunk, id)
                    .expect("peer must hold a chunk it advertised");
                self.to
                    .put_blob(BlobKind::DataChunk, &b)
                    .map_err(ConvergenceError::from)?;
            }
        }
        Ok(())
    }
}

fn converge_on(
    exec_dev: &mut Dev,
    local: &SnapshotOutput,
    remote_snap: &SnapshotOutput,
    base: Option<&ferry_store::manifest::RootManifest>,
    other: &Dev,
) -> ConvergenceResult {
    let mut fetch = PeerFetch {
        from: &other.store,
        to: &exec_dev.store,
    };
    let result = ConvergenceEngine::new(&exec_dev.store, &exec_dev.tree)
        .state_dir(&exec_dev.state)
        .at(NOW)
        .fetch_with(&mut fetch)
        .converge(&local.manifest, &remote_snap.manifest, base);
    result.unwrap_or_else(|e| panic!("converge failed on {}: {e}", hex(&exec_dev.dev)))
}

fn record_agreement(recorder: &mut Dev, peer: [u8; 32], manifest_id: BlobId) {
    ferry_store::agreement::AgreementLedger::new(&recorder.state)
        .record(
            &FOLDER,
            &ferry_store::agreement::AgreedRecord {
                peer_device_id: peer,
                manifest_id,
                agreed_sec: NOW.0,
                agreed_nsec: 0,
            },
        )
        .unwrap();
}

fn load_agreed(d: &Dev, peer: [u8; 32]) -> Option<ferry_store::agreement::AgreedRecord> {
    ferry_store::agreement::AgreementLedger::new(&d.state)
        .get(&FOLDER, &peer)
        .unwrap()
}

fn run_case(scenario: Scenario, direction: Direction, order: Order) {
    let mut a = Dev::new(1, DEV_A, poly(42));
    let mut b = Dev::new(2, DEV_B, poly(42));
    let path = "f.txt";

    let has_base = matches!(scenario, Scenario::BothChange | Scenario::DeleteVsEdit);
    if has_base {
        write_file(&a.tree.join(path), b"v0", (1000, 0));
        write_file(&b.tree.join(path), b"v0", (1000, 0));
        let s0 = a.snap();

        transfer_meta(&a.store, &b.store, &s0);
        b.parent = s0.manifest_id;
        record_agreement(&mut a, DEV_B, s0.manifest_id);
        record_agreement(&mut b, DEV_A, s0.manifest_id);
    }

    let (va_bytes, vb_bytes): (&[u8], &[u8]) = (b"version A", b"version B");
    match scenario {
        Scenario::BothChange | Scenario::AddVsAddDiff => {
            let (mt_a, mt_b) = match direction {
                Direction::AWins => ((3000i64, 700), (2900i64, 500)),
                Direction::BWins => ((2900i64, 700), (3000i64, 500)),
                Direction::Tie => ((2950i64, 777), (2950i64, 777)),
            };
            write_file(&a.tree.join(path), va_bytes, mt_a);
            write_file(&b.tree.join(path), vb_bytes, mt_b);
        }
        Scenario::AddVsAddSame => {
            let (mt_a, mt_b) = match direction {
                Direction::AWins => ((3100i64, 700), (3000i64, 500)),
                Direction::BWins => ((3000i64, 700), (3100i64, 500)),
                Direction::Tie => ((3050i64, 777), (3050i64, 777)),
            };
            write_file(&a.tree.join(path), b"same content", mt_a);
            write_file(&b.tree.join(path), b"same content", mt_b);
        }
        Scenario::DeleteVsEdit => {
            let edit = |d: &mut Dev| write_file(&d.tree.join(path), b"the edit", (3200i64, 5));
            match direction {
                Direction::AWins => {
                    std::fs::remove_file(b.tree.join(path)).unwrap();
                    edit(&mut a);
                }

                Direction::BWins => {
                    std::fs::remove_file(a.tree.join(path)).unwrap();
                    edit(&mut b);
                }

                Direction::Tie => {
                    edit(&mut a);
                    edit(&mut b);
                }
            }
        }
    }

    let sa = a.snap();
    let sb = b.snap();

    let truth: HashSet<Vec<u8>> = {
        let mut t = collect_bytes(&a.tree);
        t.extend(collect_bytes(&b.tree));
        t
    };

    let base_a = load_base_object(&a, load_agreed(&a, DEV_B).map(|r| r.manifest_id));
    let base_b = load_base_object(&b, load_agreed(&b, DEV_A).map(|r| r.manifest_id));

    match order {
        Order::AFirst => {
            transfer_meta(&b.store, &a.store, &sb);
            converge_on(&mut a, &sa, &sb, base_a.as_ref(), &b);
            let sa2 = a.snap();
            transfer_meta(&a.store, &b.store, &sa2);
            converge_on(&mut b, &sb, &sa2, base_b.as_ref(), &a);
        }
        Order::BFirst => {
            transfer_meta(&a.store, &b.store, &sa);
            converge_on(&mut b, &sb, &sa, base_b.as_ref(), &a);
            let sb2 = b.snap();
            transfer_meta(&b.store, &a.store, &sb2);
            converge_on(&mut a, &sa, &sb2, base_a.as_ref(), &b);
        }
        Order::Simultaneous => {
            transfer_meta(&a.store, &b.store, &sa);
            transfer_meta(&b.store, &a.store, &sb);
            converge_on(&mut a, &sa, &sb, base_a.as_ref(), &b);
            converge_on(&mut b, &sb, &sa, base_b.as_ref(), &a);
        }
    }

    let mut rounds = 1;
    loop {
        let sa2 = a.snap();
        let sb2 = b.snap();
        if sa2.root_tree_id == sb2.root_tree_id {
            break;
        }
        rounds += 1;
        assert!(
            rounds <= 6,
            "no convergence in {scenario:?} {direction:?} {order:?}"
        );
        transfer_meta(&a.store, &b.store, &sa2);
        transfer_meta(&b.store, &a.store, &sb2);
        let ba = load_base_object(&a, load_agreed(&a, DEV_B).map(|r| r.manifest_id));
        let bb = load_base_object(&b, load_agreed(&b, DEV_A).map(|r| r.manifest_id));
        converge_on(&mut a, &sa2, &sb2, ba.as_ref(), &b);
        converge_on(&mut b, &sb2, &sa2, bb.as_ref(), &a);
    }

    for d_label in ["A", "B"] {
        let got = match d_label {
            "A" => collect_bytes(&a.tree),
            _ => collect_bytes(&b.tree),
        };
        let missing: Vec<Vec<u8>> = truth.difference(&got).cloned().collect();
        assert!(
            missing.is_empty(),
            "{scenario:?} {direction:?} {order:?}: device {d_label} LOST {missing:?}"
        );
    }

    let (winner_bytes, loser_bytes): (Vec<u8>, Vec<u8>) = match scenario {
        Scenario::BothChange | Scenario::AddVsAddDiff => match direction {
            Direction::AWins => (va_bytes.to_vec(), vb_bytes.to_vec()),
            Direction::BWins | Direction::Tie => (vb_bytes.to_vec(), va_bytes.to_vec()),
        },
        Scenario::AddVsAddSame => (b"same content".to_vec(), b"same content".to_vec()),
        Scenario::DeleteVsEdit => (b"the edit".to_vec(), b"the edit".to_vec()),
    };

    for d_label in ["A", "B"] {
        let tree = match d_label {
            "A" => &a.tree,
            _ => &b.tree,
        };
        assert_eq!(
            std::fs::read(tree.join(path)).unwrap(),
            winner_bytes,
            "{scenario:?} {direction:?} {order:?}: wrong live bytes on {d_label}"
        );
    }

    match scenario {
        Scenario::BothChange | Scenario::AddVsAddDiff => {
            for d_label in ["A", "B"] {
                let tree = match d_label {
                    "A" => &a.tree,
                    _ => &b.tree,
                };
                let qs = quarantine_names(tree);
                assert_eq!(
                    qs.len(),
                    1,
                    "{scenario:?} {direction:?} {order:?}: expected the loser copy on {d_label}, got {qs:?}"
                );

                let q = &qs[0];
                assert_eq!(
                    std::fs::read(tree.join(q)).unwrap(),
                    loser_bytes,
                    "quarantine copy must hold the loser bytes"
                );
                let loser_short = match direction {
                    Direction::AWins => hex(&DEV_B)[..8].to_string(),
                    Direction::BWins | Direction::Tie => hex(&DEV_A)[..8].to_string(),
                };
                assert!(
                    q.contains(&format!(".ferry-conflict.{loser_short}-")),
                    "{q}"
                );
            }
        }
        Scenario::DeleteVsEdit | Scenario::AddVsAddSame => {
            for tree in [&a.tree, &b.tree] {
                assert!(
                    quarantine_names(tree).is_empty(),
                    "no quarantine files expected"
                );
            }
        }
    }

    let no_conflict_at_all = match scenario {
        Scenario::AddVsAddSame => true,
        Scenario::DeleteVsEdit if direction == Direction::Tie => true,
        _ => false,
    };
    let expect_total = if no_conflict_at_all {
        0
    } else {
        let winner_is_a = direction == Direction::AWins;
        match order {
            Order::Simultaneous => 2,
            Order::AFirst => {
                if winner_is_a {
                    2
                } else {
                    1
                }
            }
            Order::BFirst => {
                if winner_is_a {
                    1
                } else {
                    2
                }
            }
        }
    };
    let mut total_entries = 0;
    for (d_label, d) in [("A", &a), ("B", &b)] {
        let entries = list_conflicts(&d.state).unwrap();
        total_entries += entries.len();
        for e in &entries {
            let second_executor_degraded = match order {
                Order::Simultaneous => false,
                Order::AFirst => d_label == "B",
                Order::BFirst => d_label == "A",
            };
            let want_kind = match (scenario, second_executor_degraded) {
                (Scenario::DeleteVsEdit, _) => "delete_vs_edit",
                (Scenario::AddVsAddDiff, _) => "add_vs_add",
                (_, true) => "add_vs_add",
                (_, false) => "both_changed",
            };
            assert_eq!(e.kind, want_kind, "{d_label}");
            assert_eq!(e.path, path);
            assert_eq!(
                e.winner.device,
                winner_device_hex(direction, scenario),
                "{d_label}"
            );
            assert_eq!(
                e.quarantined_as.is_some(),
                !matches!(scenario, Scenario::DeleteVsEdit)
            );
            assert_eq!(e.folder_id, hex(&FOLDER));
        }

        let raw = std::fs::read_to_string(d.state.join("conflicts.jsonl")).unwrap_or_default();
        assert_eq!(raw.lines().count(), entries.len());
        for line in raw.lines() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("winner").and_then(|w| w.get("device")).is_some());
        }
    }
    assert_eq!(
        total_entries, expect_total,
        "{scenario:?} {direction:?} {order:?}: report totals"
    );

    let fa = a.snap();
    let fb = b.snap();
    assert_eq!(fa.root_tree_id, fb.root_tree_id, "manifests must converge");
    transfer_meta(&a.store, &b.store, &fa);
    transfer_meta(&b.store, &a.store, &fb);
    let ba = load_base_object(&a, load_agreed(&a, DEV_B).map(|r| r.manifest_id));
    let bb = load_base_object(&b, load_agreed(&b, DEV_A).map(|r| r.manifest_id));
    let ra = converge_on(&mut a, &fa, &fb, ba.as_ref(), &b);
    let rb = converge_on(&mut b, &fb, &fa, bb.as_ref(), &a);
    assert!(
        ra.is_noop() && rb.is_noop(),
        "converged devices must converge to zero-op results ({ra:?} / {rb:?})"
    );
    assert_eq!(collect_bytes(&a.tree), collect_bytes(&b.tree));

    record_agreement(&mut a, DEV_B, fa.manifest_id);
    record_agreement(&mut b, DEV_A, fb.manifest_id);
    assert_eq!(load_agreed(&a, DEV_B).unwrap().manifest_id, fa.manifest_id);

    let _ = rounds;
}

fn winner_device_hex(direction: Direction, scenario: Scenario) -> String {
    match scenario {
        Scenario::DeleteVsEdit => match direction {
            Direction::AWins | Direction::Tie => hex(&DEV_A),
            Direction::BWins => hex(&DEV_B),
        },
        _ => match direction {
            Direction::AWins => hex(&DEV_A),
            Direction::BWins | Direction::Tie => hex(&DEV_B),
        },
    }
}

#[rustfmt::skip]
mod cells {
    use super::*;
    use Scenario::*; use Direction::*; use Order::*;

    #[test] fn both_change_a_wins_a_first()      { run_case(BothChange, AWins, AFirst) }
    #[test] fn both_change_a_wins_b_first()      { run_case(BothChange, AWins, BFirst) }
    #[test] fn both_change_a_wins_simultaneous() { run_case(BothChange, AWins, Simultaneous) }
    #[test] fn both_change_b_wins_a_first()      { run_case(BothChange, BWins, AFirst) }
    #[test] fn both_change_b_wins_b_first()      { run_case(BothChange, BWins, BFirst) }
    #[test] fn both_change_b_wins_simultaneous() { run_case(BothChange, BWins, Simultaneous) }
    #[test] fn both_change_tie_a_first()         { run_case(BothChange, Tie, AFirst) }
    #[test] fn both_change_tie_b_first()         { run_case(BothChange, Tie, BFirst) }
    #[test] fn both_change_tie_simultaneous()    { run_case(BothChange, Tie, Simultaneous) }

    #[test] fn dve_a_edits_a_first()             { run_case(DeleteVsEdit, AWins, AFirst) }
    #[test] fn dve_a_edits_b_first()             { run_case(DeleteVsEdit, AWins, BFirst) }
    #[test] fn dve_a_edits_simultaneous()        { run_case(DeleteVsEdit, AWins, Simultaneous) }
    #[test] fn dve_b_edits_a_first()             { run_case(DeleteVsEdit, BWins, AFirst) }
    #[test] fn dve_b_edits_b_first()             { run_case(DeleteVsEdit, BWins, BFirst) }
    #[test] fn dve_b_edits_simultaneous()        { run_case(DeleteVsEdit, BWins, Simultaneous) }
    #[test] fn dve_both_edit_identical_a_first() { run_case(DeleteVsEdit, Tie, AFirst) }
    #[test] fn dve_both_edit_identical_b_first() { run_case(DeleteVsEdit, Tie, BFirst) }
    #[test] fn dve_both_edit_identical_simul()   { run_case(DeleteVsEdit, Tie, Simultaneous) }

    #[test] fn add_same_a_newer_a_first()        { run_case(AddVsAddSame, AWins, AFirst) }
    #[test] fn add_same_a_newer_b_first()        { run_case(AddVsAddSame, AWins, BFirst) }
    #[test] fn add_same_a_newer_simultaneous()   { run_case(AddVsAddSame, AWins, Simultaneous) }
    #[test] fn add_same_b_newer_a_first()        { run_case(AddVsAddSame, BWins, AFirst) }
    #[test] fn add_same_b_newer_b_first()        { run_case(AddVsAddSame, BWins, BFirst) }
    #[test] fn add_same_b_newer_simultaneous()   { run_case(AddVsAddSame, BWins, Simultaneous) }
    #[test] fn add_same_equal_mtimes_all()       { run_case(AddVsAddSame, Tie, Simultaneous) }

    #[test] fn add_diff_a_wins_a_first()         { run_case(AddVsAddDiff, AWins, AFirst) }
    #[test] fn add_diff_a_wins_b_first()         { run_case(AddVsAddDiff, AWins, BFirst) }
    #[test] fn add_diff_a_wins_simultaneous()    { run_case(AddVsAddDiff, AWins, Simultaneous) }
    #[test] fn add_diff_b_wins_a_first()         { run_case(AddVsAddDiff, BWins, AFirst) }
    #[test] fn add_diff_b_wins_b_first()         { run_case(AddVsAddDiff, BWins, BFirst) }
    #[test] fn add_diff_b_wins_simultaneous()    { run_case(AddVsAddDiff, BWins, Simultaneous) }
    #[test] fn add_diff_tie_simultaneous()       { run_case(AddVsAddDiff, Tie, Simultaneous) }
}
