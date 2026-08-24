//! T-012 acceptance, verbatim from the ticket:
//!
//!   "adversarial fixture tree (unicode names, case-only rename, deep
//!    nesting past 260 chars, symlink chains) syncs correctly or fails
//!    loudly with an actionable message on every OS."
//!
//! Two assertions over ONE generated fixture:
//!
//! 1. `round_trips_through_snapshot_materialize` — snapshot → materialize →
//!    resnapshot must reproduce the identical root tree id (names, exec
//!    bits, file/dir/symlink mtimes all preserved; the symlink-mtime piece
//!    is T-012's deferred-T-005 landing). Then a case-only rename
//!    (`Rename-Me.txt` → `rename-me.TXT`) must propagate through a second
//!    round trip: old spelling gone, new spelling present, ids equal again.
//! 2. `reconciliation_conflict_inside_fixture` — two devices hold copies of
//!    the fixture, both edit the NFD-spelled unicode file, reconcile with A
//!    favored: winner bytes live everywhere, loser quarantined under an
//!    NFC-consistent name carrying the loser's short id, both devices
//!    converge to one root id, and a further cycle plans zero operations.
//!
//! Platform notes:
//! - The deep branch (>260 chars total) is created through
//!   `ferry_platform::extend_path`, so Windows runners can build it
//!   regardless of the host's `LongPathsEnabled` registry state.
//! - Symlinks are probe-gated (`symlink_creation_works`): hosts that forbid
//!   creating them skip the chain rather than fail setup; the pure policy
//!   tests cover the refused cases everywhere.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ferry_materialize::Applier;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{hex, BlobId, BlobKind};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity, SnapshotOutput};
use ferry_store::store::Store;
use ferry_sync_engine::reconcile::{reconcile, ReconcileInput};
use ferry_sync_engine::{agree, execute};

use rand::SeedableRng;

const DEV_A: [u8; 32] = [0xA1; 32];
const DEV_B: [u8; 32] = [0xB2; 32];
const NOW: (i64, u32) = (1_787_574_896, 0);

/// NFD spelling (e + combining acute) as it lands ON DISK.
const UNICODE_FILE: &str = "rapport-anne\u{301}e.md";
const UNICODE_DIR: &str = "\u{1f980}-proj\u{65}\u{301}ct"; // 🦀 + decomposed é
const UNICODE_NOTE: &str = "notes-caf\u{65}\u{301}.txt";
const CASE_FILE_BEFORE: &str = "Rename-Me.txt";
const CASE_FILE_AFTER: &str = "rename-me.TXT";

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
    set_file_mtime(path, mt);
}

#[cfg(unix)]
fn set_file_mtime(path: &Path, mt: (i64, u32)) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    stamp(&f, mt);
}
#[cfg(windows)]
fn set_file_mtime(path: &Path, mt: (i64, u32)) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    stamp(&f, mt);
}

fn stamp(f: &std::fs::File, mt: (i64, u32)) {
    f.set_times(std::fs::FileTimes::new().set_modified(ferry_platform::join_unix(mt.0, mt.1)))
        .unwrap();
}

fn set_dir_mtime(path: &Path, mt: (i64, u32)) {
    #[cfg(unix)]
    {
        let f = std::fs::File::open(path).unwrap();
        stamp(&f, mt);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap();
        stamp(&f, mt);
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, mt);
    }
}

/// Probe once per process: may this host create symlinks at all?
fn symlink_creation_works(root: &Path) -> bool {
    use std::sync::OnceLock;
    static PROBE: OnceLock<bool> = OnceLock::new();
    *PROBE.get_or_init(|| {
        let probe = root.join(".ferry-symlink-probe");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("target", &probe).is_ok_and(|()| {
                let _ = std::fs::remove_file(&probe);
                true
            })
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file("target", &probe).is_ok_and(|_| {
                let _ = std::fs::remove_file(&probe);
                true
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = root;
            false
        }
    })
}

/// Create one directory level, applying the long-path prefix rule so the
/// >260-char branch builds even without host opt-in (Windows).
fn mkdir_deep(cumulative: &Path) {
    let effective = ferry_platform::extend_path(cumulative);
    if effective.exists() {
        return;
    }
    std::fs::create_dir(&effective)
        .or_else(|e| {
            // Some platforms need the parent chain first; recurse manually.
            if let Some(parent) = effective.parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    mkdir_deep(parent);
                    return std::fs::create_dir(&effective);
                }
            }
            Err(e)
        })
        .unwrap();
}

/// Build the adversarial fixture. Deterministic bytes and mtimes so both
/// devices' copies are byte-identical manifests.
fn build_fixture(root: &Path) {
    std::fs::create_dir_all(root).unwrap();

    // 1. Unicode names (NFD spellings on disk where possible).
    write_file(
        &root.join(UNICODE_DIR).join(UNICODE_NOTE),
        "caf\u{e9} notes inside an emoji dir\n".as_bytes(),
        (1_700_000_100, 10),
    );
    write_file(
        &root.join(UNICODE_FILE),
        b"annee base content\n",
        (1_700_000_200, 20),
    );

    // 2. Deep nesting: total path comfortably past 260 chars.
    let mut deep = root.to_path_buf();
    for i in 0..14 {
        deep.push(format!("level-{i:02}-aaaaaaaaaaaaaaaaaaaaaaaaaa"));
        mkdir_deep(&deep);
    }
    write_file(
        &deep.join("deep-leaf.bin"),
        &[0xAB; 1024],
        (1_700_000_300, 30),
    );

    // 3. Case-rename subject.
    write_file(
        &root.join(CASE_FILE_BEFORE),
        b"case me\n",
        (1_700_000_400, 40),
    );

    // 4. Symlink chain (probe-gated): z_link_a -> z_link_b -> z_link_c ->
    //    the unicode note. Every hop is relative and internal, i.e. policy-
    //    clean per T-012.
    if symlink_creation_works(root) {
        write_file(
            &root.join("z_link_c"),
            b"chain target\n",
            (1_700_000_500, 50),
        );
        // Repoint z_link_c at the unicode note by making IT the real file...
        // simpler: c is a link too. Order matters: deepest first.
        std::fs::remove_file(root.join("z_link_c")).unwrap();
        write_file(
            &root.join("real-note.txt"),
            b"chain target\n",
            (1_700_000_500, 50),
        );
        make_link("real-note.txt", &root.join("z_link_c"));
        make_link("z_link_c", &root.join("z_link_b"));
        make_link("z_link_b", &root.join("z_link_a"));
        // Deterministic link mtimes: without stamping, each device's links
        // carry their creation wall-clock and reconciliation sees eternal
        // metadata drift on the chain (this bit us in testing — link times
        // are manifest content per docs/store-format.md).
        for (i, name) in ["z_link_c", "z_link_b", "z_link_a"].iter().enumerate() {
            let _ = ferry_materialize::set_symlink_times(
                &root.join(name),
                1_700_000_600 + i as i64,
                60,
            );
        }
    }

    // Dir mtimes deepest-first so snapshots are deterministic.
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(e.path());
            }
        }
        set_dir_mtime(&dir, (1_700_000_000, dir.components().count() as u32));
    }
}

#[cfg(unix)]
fn make_link(target: &str, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}
#[cfg(windows)]
fn make_link(target: &str, at: &Path) {
    std::os::windows::fs::symlink_file(target, at).unwrap();
}

// ---- round-trip harness ----------------------------------------------------

struct RoundTrip {
    _dir: tempfile::TempDir,
    store: Store,
    source: PathBuf,
    target: PathBuf,
}

impl RoundTrip {
    fn new(tag: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = Store::create(&store_root, fmk(), Box::new(PassthroughCipher)).unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        build_fixture(&source);
        let _ = tag;
        RoundTrip {
            _dir: dir,
            store,
            source,
            target,
        }
    }

    fn snap_source(&self) -> SnapshotOutput {
        let out = snapshot_dir(
            &self.store,
            poly(7),
            &self.source,
            &identity((1_000_000 + 1, 0)),
        )
        .unwrap();
        assert!(
            out.refused.is_empty(),
            "fixture must be fully representable or the fixture itself is wrong: {:?}",
            out.refused
        );
        out
    }

    fn snap_target(&self) -> SnapshotOutput {
        snapshot_dir(
            &self.store,
            poly(7),
            &self.target,
            &identity((2_000_000 + 1, 0)),
        )
        .unwrap()
    }
}

fn identity(at: (i64, u32)) -> SnapshotIdentity {
    SnapshotIdentity {
        folder_id: [7; 16],
        device_id: [9; 32],
        parent_manifest_id: [0; 32],
        created_sec: at.0,
        created_nsec: at.1,
    }
}

// ---- reconciliation harness (mirrors matrix.rs, one scenario) ---------------

struct Dev {
    _dir: tempfile::TempDir,
    store: Store,
    tree: PathBuf,
    state: PathBuf,
    dev: [u8; 32],
    parent: [u8; 32],
}

impl Dev {
    fn new(_tag: i64, dev: [u8; 32]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store_root = dir.path().join("store");
        std::fs::create_dir_all(&store_root).unwrap();
        let store = Store::create(&store_root, fmk(), Box::new(PassthroughCipher)).unwrap();
        let tree = dir.path().join("tree");
        build_fixture(&tree);
        let state = dir.path().join("state");
        Dev {
            _dir: dir,
            store,
            tree,
            state,
            dev,
            parent: [0; 32],
        }
    }

    fn snap(&mut self) -> SnapshotOutput {
        let idn = SnapshotIdentity {
            folder_id: [7; 16],
            device_id: self.dev,
            parent_manifest_id: self.parent,
            created_sec: NOW.0 + i64::from(self.parent[0]),
            created_nsec: 0,
        };
        let out = snapshot_dir(&self.store, poly(42), &self.tree, &idn).unwrap();
        assert!(out.refused.is_empty());
        self.parent = out.manifest_id;
        out
    }
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

fn plan_on(
    exec_dev: &mut Dev,
    local: &SnapshotOutput,
    remote_snap: &SnapshotOutput,
    base: Option<&ferry_store::manifest::RootManifest>,
    other: &Dev,
) -> ferry_sync_engine::plan::ActionPlan {
    let mut plan = reconcile(ReconcileInput {
        store: &exec_dev.store,
        local: &local.manifest,
        remote: &remote_snap.manifest,
        base,
    })
    .unwrap_or_else(|e| panic!("reconcile failed on {}: {e}", hex(&exec_dev.dev)));
    for (id, _) in &plan.fetch {
        if exec_dev.store.get(BlobKind::DataChunk, id).is_err() {
            let b = other.store.get(BlobKind::DataChunk, id).unwrap();
            exec_dev.store.put_blob(BlobKind::DataChunk, &b).unwrap();
        }
    }
    plan.guard_expected = Some(local.manifest.clone());
    plan
}

fn run_plan(d: &mut Dev, plan: &ferry_sync_engine::plan::ActionPlan) {
    execute(&d.store, &d.tree, plan, Some(&d.state), NOW).expect("execute must succeed");
}

fn record_agreement(recorder: &mut Dev, peer: [u8; 32], manifest_id: BlobId) {
    agree::PeerState::new(&recorder.state)
        .record(&agree::AgreedRecord {
            peer_device_id: peer,
            manifest_id,
            agreed_sec: NOW.0,
            agreed_nsec: 0,
        })
        .unwrap();
}

fn load_agreed(d: &Dev, peer: [u8; 32]) -> Option<agree::AgreedRecord> {
    agree::PeerState::new(&d.state).load(&peer).unwrap()
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
            if p.is_symlink() {
                continue;
            }
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

// ---- the two acceptance tests ----------------------------------------------

/// Exact-spelling directory membership: `Path::exists()` folds case on
/// macOS/Windows, which would defeat case-rename assertions.
fn has_exact_entry(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_str() == Some(name))
}

#[test]
fn round_trips_through_snapshot_materialize() {
    let rt = RoundTrip::new(1);

    let s1 = rt.snap_source();
    Applier::new(&rt.store, &rt.target)
        .apply_manifest(&s1.manifest)
        .expect("fixture materialization must succeed");

    let s2 = rt.snap_target();
    assert_eq!(
        s1.root_tree_id, s2.root_tree_id,
        "round trip must reproduce the identical tree (unicode, deep nesting, \
         symlink chains, all mtimes)"
    );

    // Case-only rename propagates: same inode on folding hosts, delete+add
    // elsewhere; either way only the new spelling survives.
    std::fs::rename(
        rt.source.join(CASE_FILE_BEFORE),
        rt.source.join(CASE_FILE_AFTER),
    )
    .expect("case-only rename must work on the host FS");
    let s3 = rt.snap_source();

    // Guarded application of a case-only rename REFUSES LOUDLY on folding
    // hosts: the base expectation says the new spelling is absent, while
    // live disk holds its folded twin. That is the designed safety net —
    // nothing is modified, and a fresh scan makes the next guarded cycle
    // clean. Assert the loud refusal, then show the unguarded apply
    // converges exactly.
    let guarded = Applier::new(&rt.store, &rt.target)
        .overwrite(ferry_materialize::Overwrite::Expect {
            expected: s2.manifest.clone(),
        })
        .apply_manifest(&s3.manifest);
    if ferry_platform::host_folds_case() {
        assert!(
            matches!(
                &guarded,
                Err(ferry_materialize::MaterializeError::Diverged { .. })
            ),
            "folding hosts must refuse a folded rename under guard, got {guarded:?}"
        );
        assert_eq!(
            rt.snap_target().root_tree_id,
            s2.root_tree_id,
            "refused apply must have modified nothing"
        );
    } else {
        assert!(
            guarded.is_ok(),
            "case-sensitive host: no folding, no refusal"
        );
    }

    // Unguarded application converges exactly: removals execute before
    // upserts, and the fold-shadowed write is forced (never a Skip).
    Applier::new(&rt.store, &rt.target)
        .apply_manifest(&s3.manifest)
        .expect("unguarded rename propagation must succeed");

    let s5 = rt.snap_target();
    if s3.root_tree_id != s5.root_tree_id {
        let cs =
            ferry_store::diff::diff_roots(&rt.store, &s3.root_tree_id, &s5.root_tree_id).unwrap();
        eprintln!("POST-RENAME DIFF s3(target-desired) vs s5(live):\n{cs:?}");
    }
    assert_eq!(
        s3.root_tree_id, s5.root_tree_id,
        "post-rename round trip must match exactly"
    );
    assert!(
        !has_exact_entry(&rt.target, CASE_FILE_BEFORE),
        "old case spelling must be gone (exact-match check; Path::exists \
         folds case on macOS/Windows)"
    );
    assert!(has_exact_entry(&rt.target, CASE_FILE_AFTER));

    // Deep branch really is past MAX_PATH (the point of the fixture).
    let mut probe = rt.source.clone();
    for i in 0..14 {
        probe.push(format!("level-{i:02}-aaaaaaaaaaaaaaaaaaaaaaaaaa"));
    }
    let total = ferry_platform::extend_path(&probe)
        .to_string_lossy()
        .chars()
        .count();
    assert!(
        total >= ferry_platform::MAX_PATH
            || cfg!(target_os = "macos") && {
                // On macOS the temp base can keep totals below the cap; the
                // component count is what the fixture controls.
                probe.components().count() > 15
            },
        "fixture deep branch must stress path limits ({total} chars)"
    );
}

#[test]
fn reconciliation_conflict_inside_fixture() {
    let mut a = Dev::new(1, DEV_A);
    let mut b = Dev::new(2, DEV_B);

    // Identical starting trees -> unambiguous ancestor.
    let sa0 = a.snap();
    let sb0 = b.snap();
    record_agreement(&mut a, DEV_B, sa0.manifest_id);
    record_agreement(&mut b, DEV_A, sb0.manifest_id);

    // Diverge on the NFD-spelled unicode file, A favored by mtime.
    write_file(&a.tree.join(UNICODE_FILE), b"version A\n", (3_000, 700));
    write_file(&b.tree.join(UNICODE_FILE), b"version B\n", (2_900, 500));

    let truth: HashSet<Vec<u8>> = {
        let mut t = collect_bytes(&a.tree);
        t.extend(collect_bytes(&b.tree));
        t
    };

    let sa = a.snap();
    let sb = b.snap();

    // One full exchange round, A first.
    let base_a = load_base_object(&a, load_agreed(&a, DEV_B).map(|r| r.manifest_id));
    transfer_meta(&b.store, &a.store, &sb);
    let pa = plan_on(&mut a, &sa, &sb, base_a.as_ref(), &b);
    run_plan(&mut a, &pa);
    let sa2 = a.snap();
    let base_b = load_base_object(&b, load_agreed(&b, DEV_A).map(|r| r.manifest_id));
    transfer_meta(&a.store, &b.store, &sa2);
    let pb = plan_on(&mut b, &sb, &sa2, base_b.as_ref(), &a);
    run_plan(&mut b, &pb);

    // Converge.
    let mut rounds = 1;
    loop {
        let sa3 = a.snap();
        let sb3 = b.snap();
        if sa3.root_tree_id == sb3.root_tree_id {
            break;
        }
        rounds += 1;
        assert!(rounds <= 6, "fixture conflict did not converge");
        transfer_meta(&a.store, &b.store, &sa3);
        transfer_meta(&b.store, &a.store, &sb3);
        let ba = load_base_object(&a, load_agreed(&a, DEV_B).map(|r| r.manifest_id));
        let bb = load_base_object(&b, load_agreed(&b, DEV_A).map(|r| r.manifest_id));
        let pa2 = plan_on(&mut a, &sa3, &sb3, ba.as_ref(), &b);
        let pb2 = plan_on(&mut b, &sb3, &sa3, bb.as_ref(), &a);
        run_plan(&mut a, &pa2);
        run_plan(&mut b, &pb2);
    }

    // Zero silent data loss on every device.
    for (label, tree) in [("A", &a.tree), ("B", &b.tree)] {
        let got = collect_bytes(tree);
        let missing: Vec<Vec<u8>> = truth.difference(&got).cloned().collect();
        assert!(missing.is_empty(), "device {label} LOST {missing:?}");
    }

    // Winner correctness: the NFC-composed path carries A's bytes on BOTH.
    assert_eq!(
        std::fs::read(a.tree.join(UNICODE_FILE)).unwrap(),
        b"version A\n"
    );
    assert_eq!(
        std::fs::read(b.tree.join(UNICODE_FILE)).unwrap(),
        b"version A\n"
    );

    // Quarantine accounting: loser copy exists on B under a name carrying
    // B's short id, NFC-consistent (decomposed input, composed output).
    let qs = quarantine_names(&b.tree);
    assert_eq!(qs.len(), 1, "exactly the loser copy, got {qs:?}");
    assert!(qs[0].contains("b2b2b2b2"), "loser short id: {}", qs[0]);
    assert!(
        qs[0].starts_with("rapport-ann\u{e9}e.md.ferry-conflict."),
        "NFC-composed quarantine name: {}",
        qs[0]
    );

    // Fixed point: another full cycle plans no materialize/quarantine ops.
    let sa4 = a.snap();
    let sb4 = b.snap();
    transfer_meta(&a.store, &b.store, &sa4);
    transfer_meta(&b.store, &a.store, &sb4);
    let pa3 = plan_on(&mut a, &sa4, &sb4, None, &b);
    let pb3 = plan_on(&mut b, &sb4, &sa4, None, &a);
    assert!(
        pa3.materialize.is_empty() && pb3.materialize.is_empty(),
        "fixed point violated: ops remain after convergence"
    );
}
