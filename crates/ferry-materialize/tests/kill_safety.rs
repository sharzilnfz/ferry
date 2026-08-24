//! Crash-safety proof (T-005 acceptance): SIGKILL the materializer at
//! uniformly random points and require the tree to stay CONSISTENT.
//!
//! Consistency means, per path (the atomicity unit of temp+rename):
//!
//! - every PRESENT file's bytes re-chunk under the folder polynomial to the
//!   exact chunk-id sequence of EITHER the old or the new state for that
//!   path (so no partially-written destination file can hide), with the
//!   exec bit matching whichever state matched;
//! - every present directory/symlink exists in and matches one of the two
//!   states; nothing outside the two states' universe exists;
//! - every remaining temp file fits the documented temp-name pattern.
//!
//! A mid-apply kill legitimately leaves SOME paths already updated and
//! others not yet touched, so the tree as a whole is generally neither the
//! old nor the new state; the invariant being proven is that each path
//! individually is always wholly-old or wholly-new, never hybrid.
//!
//! Harness self-tests guard against a vacuous checker: a completed apply
//! must equal the new state exactly, and a deliberately corrupted byte must
//! be rejected.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::time::Duration;

// Kill-loop-only: the SIGKILL harness below is unix-only (libc::kill) and
// is the sole user of Duration, Rng (gen_range), KILL_ITERATIONS and
// SEED_BASE. setup_world seeds the folder polynomial on every platform, so
// StdRng/SeedableRng must stay available unconditionally.
use rand::rngs::StdRng;
#[cfg(unix)]
use rand::Rng;
use rand::SeedableRng;

use ferry_materialize::temp::is_temp_name;
use ferry_store::chunker::{chunk, generate_polynomial};
use ferry_store::crypto::PassthroughCipher;
use ferry_store::format::{BlobId, BlobKind};
use ferry_store::manifest::{dir_entry, file_entry, serialize_tree_node, symlink_entry, TreeNode};
use ferry_store::store::Store;

const FMK: [u8; 32] = [42u8; 32];
#[cfg(unix)]
const KILL_ITERATIONS: usize = 25;
#[cfg(unix)]
const SEED_BASE: u64 = 0x5EED_0001;

// ---------------------------------------------------------------------------
// Models: the complete desired content of a tree, independent of the store.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FileSpec {
    bytes: Vec<u8>,
    exec: bool,
}

#[derive(Clone, Default)]
struct Model {
    /// rel path ("/"-joined) -> spec. Includes files inside dirs but NOT
    /// dir entries themselves.
    files: BTreeMap<String, FileSpec>,
    dirs: BTreeSet<String>,
    symlinks: BTreeMap<String, String>,
}

impl Model {
    fn add_file(&mut self, rel: &str, bytes: Vec<u8>, exec: bool) {
        self.files.insert(rel.to_string(), FileSpec { bytes, exec });
        register_parents(&mut self.dirs, rel);
    }

    fn add_dir(&mut self, rel: &str) {
        self.dirs.insert(rel.to_string());
    }

    fn add_symlink(&mut self, rel: &str, target: &str) {
        self.symlinks.insert(rel.to_string(), target.to_string());
    }
}

fn register_parents(dirs: &mut BTreeSet<String>, rel: &str) {
    let mut cut = rel.len();
    while let Some(pos) = rel[..cut].rfind('/') {
        dirs.insert(rel[..pos].to_string());
        cut = pos;
    }
}

fn parent_of(rel: &str) -> Option<&str> {
    rel.rfind('/').map(|p| &rel[..p])
}

fn base_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Deterministic pseudo-random payload of a given size.
fn prng_bytes(seed: u64, len: usize) -> Vec<u8> {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    let mut rng = StdRng::seed_from_u64(seed);
    (0..len).map(|_| rng.gen()).collect()
}

// ---------------------------------------------------------------------------
// World construction: one store, two states, old seeded directly on disk.
// ---------------------------------------------------------------------------

const KIB: usize = 1024;

struct WorldRoot {
    _dir: tempfile::TempDir,
    store_folder: PathBuf,
    poly: u64,
}

/// Build the two states. `old` is materialized onto disk by the caller per
/// iteration; `new` becomes a stored tree.
fn build_models() -> (Model, Model) {
    let mut old = Model::default();
    let mut new = Model::default();

    // Shared scaffolding present in both states (symlink targets).
    for m in [&mut old, &mut new] {
        m.add_file("a/target.txt", b"link target one".to_vec(), false);
        m.add_file("a/other.txt", b"link target two".to_vec(), false);
    }
    old.add_symlink("lnk", "a/target.txt");
    new.add_symlink("lnk", "a/other.txt");

    // Kept byte-identical across states.
    for (i, size) in [64 * KIB, 200 * KIB, 64 * KIB].iter().enumerate() {
        let bytes = prng_bytes(1000 + i as u64, *size);
        let exec = i % 2 == 0;
        old.add_file(&format!("same{i}.bin"), bytes.clone(), exec);
        new.add_file(&format!("same{i}.bin"), bytes, exec);
    }

    // Modified in place (multi-chunk so kills can land between chunks).
    for i in 0..2u64 {
        let o = prng_bytes(2000 + i, 600 * KIB);
        let n = prng_bytes(3000 + i, 600 * KIB + 777);
        old.add_file(&format!("mod{i}.bin"), o, false);
        new.add_file(&format!("mod{i}.bin"), n, i == 1); // exec flips too
    }

    // A big three-ish-chunk file whose rewrite dominates the timeline.
    old.add_file("big.bin", prng_bytes(4001, 1400 * KIB), false);
    new.add_file("big.bin", prng_bytes(4002, 1500 * KIB), true);

    // Deleted by the new state.
    old.add_file("gone.txt", b"delete me".to_vec(), false);
    old.add_dir("z");
    old.add_file("z/deep.txt", b"old subtree".to_vec(), true);

    // Type changes in both directions.
    old.add_file("tc", b"was a file".to_vec(), true);
    new.add_dir("tc");
    new.add_file("tc/child.txt", b"now a dir".to_vec(), false);
    old.add_dir("tcd");
    old.add_file("tcd/inner.txt", b"was a dir".to_vec(), false);
    new.add_file("tcd", b"now a file".to_vec(), true);

    // Brand-new in the new state.
    new.add_file("brand-new.bin", prng_bytes(5001, 64 * KIB), false);
    new.add_dir("n");
    new.add_file("n/x.bin", prng_bytes(5002, 130 * KIB), true);

    (old, new)
}

/// Write `model` straight to disk (plain writes; only used for the OLD
/// state, which needs no store backing).
fn seed_target(target: &Path, model: &Model) {
    if target.exists() {
        std::fs::remove_dir_all(target).unwrap();
    }
    std::fs::create_dir_all(target).unwrap();
    for d in &model.dirs {
        std::fs::create_dir_all(target.join(d)).unwrap();
    }
    for (rel, spec) in &model.files {
        let p = target.join(rel);
        std::fs::write(&p, &spec.bytes).unwrap();
        set_exec(&p, spec.exec);
    }
    for (rel, tgt) in &model.symlinks {
        #[cfg(unix)]
        std::os::unix::fs::symlink(tgt, target.join(rel)).unwrap();
        #[cfg(not(unix))]
        let _ = (rel, tgt);
    }
}

#[cfg(unix)]
fn set_exec(p: &Path, exec: bool) {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(p).unwrap().permissions();
    perm.set_mode(if exec { 0o755 } else { 0o644 });
    std::fs::set_permissions(p, perm).unwrap();
}
#[cfg(not(unix))]
fn set_exec(_p: &Path, _exec: bool) {}

/// Chunk + store every file of `model`, build tree nodes bottom-up, return
/// the root tree id.
fn store_model(w: &WorldRoot, model: &Model) -> BlobId {
    let store = open_store(w);
    let mut child_trees: HashMap<String, BlobId> = HashMap::new();

    // Deep-first over sorted dirs ensures children exist before parents.
    let mut dirs_sorted: Vec<&String> = model.dirs.iter().collect();
    dirs_sorted.sort_by_key(|d| std::cmp::Reverse(d.split('/').count()));

    fn node_for(
        w: &WorldRoot,
        store: &Store,
        model: &Model,
        prefix: Option<&str>,
        child_trees: &HashMap<String, BlobId>,
    ) -> TreeNode {
        let mut entries: Vec<ferry_store::manifest::TreeEntry> = Vec::new();
        let in_dir = |rel: &str| match prefix {
            None => !rel.contains('/'),
            Some(p) => parent_of(rel) == Some(p),
        };
        for d in &model.dirs {
            if in_dir(d) {
                let cid = child_trees[d.as_str()];
                entries.push(dir_entry(base_name(d), 1_700_000_000, 1, cid));
            }
        }
        for (rel, spec) in &model.files {
            if in_dir(rel) {
                let chunks: Vec<(BlobId, u64)> = chunk(w.poly, &spec.bytes)
                    .iter()
                    .map(|b| (store.put_data(b).unwrap(), b.len() as u64))
                    .collect();
                entries.push(file_entry(
                    base_name(rel),
                    spec.exec,
                    1_700_000_000,
                    2,
                    chunks,
                ));
            }
        }
        for (rel, tgt) in &model.symlinks {
            if in_dir(rel) {
                entries.push(symlink_entry(base_name(rel), 1_700_000_000, 3, tgt));
            }
        }
        TreeNode { entries }
    }

    for d in dirs_sorted {
        let node = node_for(w, &store, model, Some(d), &child_trees);
        let bytes = serialize_tree_node(&node);
        let id = *blake3::hash(&bytes).as_bytes();
        store.put_meta(BlobKind::TreeNode, &bytes).unwrap();
        child_trees.insert(d.clone(), id);
    }
    let root = node_for(w, &store, model, None, &child_trees);
    let root_id = *blake3::hash(serialize_tree_node(&root).as_slice()).as_bytes();
    store
        .put_meta(BlobKind::TreeNode, &serialize_tree_node(&root))
        .unwrap();
    // End-of-burst rules: seal packs and persist locations so a fresh
    // process can read everything back.
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
    root_id
}

fn setup_world() -> WorldRoot {
    let dir = tempfile::tempdir().unwrap();
    let store_folder = dir.path().join("folder");
    std::fs::create_dir_all(&store_folder).unwrap();
    let poly = generate_polynomial(&mut StdRng::seed_from_u64(7));
    // Materialize the on-disk store layout up front; later steps open it.
    Store::create(&store_folder, FMK, Box::new(PassthroughCipher)).unwrap();
    WorldRoot {
        _dir: dir,
        store_folder,
        poly,
    }
}

fn open_store(w: &WorldRoot) -> Store {
    Store::open(&w.store_folder, FMK, Box::new(PassthroughCipher)).unwrap()
}

fn apply_once_path() -> PathBuf {
    // Some cargo versions expose the example path via env var; otherwise
    // locate it in the workspace target dir (cargo test always builds
    // examples).
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_apply_once") {
        return PathBuf::from(p);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let candidate = manifest
        .join("../../target")
        .join(profile)
        .join("examples")
        .join(if cfg!(windows) {
            "apply_once.exe"
        } else {
            "apply_once"
        });
    candidate
        .canonicalize()
        .expect("apply_once example binary not found; run cargo test once more")
}

// ---------------------------------------------------------------------------
// Consistency verification
// ---------------------------------------------------------------------------

fn chunk_ids(poly: u64, bytes: &[u8]) -> Vec<BlobId> {
    chunk(poly, bytes)
        .iter()
        .map(|b| *blake3::hash(b).as_bytes())
        .collect()
}

fn live_exec(md: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = md;
        false
    }
}

/// Verify the tree under `target` is a legal crash state between the two
/// models (see module docs).
fn verify_consistent(target: &Path, old_m: &Model, new_m: &Model, poly: u64) -> Result<(), String> {
    let mut stack = vec![target.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            let p = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel_full = {
                let suffix = p
                    .strip_prefix(target)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                suffix
            };
            if is_temp_name(&name) {
                // Temp names must fit the documented pattern — is_temp_name
                // already proved that; nothing else to check on them.
                continue;
            }
            let ft = entry.file_type().map_err(|e| e.to_string())?;
            let rel = rel_full.clone();
            if ft.is_dir() {
                if rel.is_empty() {
                    stack.push(p);
                    continue;
                }
                let known = old_m.dirs.contains(&rel) || new_m.dirs.contains(&rel);
                if !known {
                    return Err(format!("unexpected directory {rel:?}"));
                }
                stack.push(p);
                continue;
            }
            if ft.is_symlink() {
                let found = std::fs::read_link(&p)
                    .map_err(|e| e.to_string())?
                    .to_string_lossy()
                    .into_owned();
                let mut candidates: Vec<&str> = vec![];
                if let Some(t) = old_m.symlinks.get(&rel) {
                    candidates.push(t);
                }
                if let Some(t) = new_m.symlinks.get(&rel) {
                    candidates.push(t);
                }
                if candidates.is_empty() {
                    return Err(format!("orphan symlink {rel:?}"));
                }
                if !candidates.iter().any(|c| c == &found) {
                    return Err(format!(
                        "symlink {rel:?} points to {found:?}, expected one of {candidates:?}"
                    ));
                }
                continue;
            }
            // Regular file: re-chunk and compare id sequences against both
            // states.
            let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
            let ids = chunk_ids(poly, &bytes);
            let exec = live_exec(&entry.metadata().map_err(|e| e.to_string())?);
            let matches = |m: &Model| match m.files.get(&rel) {
                None => false,
                Some(spec) => chunk_ids(poly, &spec.bytes) == ids && spec.exec == exec,
            };
            if !matches(old_m) && !matches(new_m) {
                let want_old = old_m.files.get(&rel);
                let want_new = new_m.files.get(&rel);
                return Err(format!(
                    "file {rel:?} matches neither state (len {}, {} chunks; \
                     old spec: {:?}, new spec: {:?})",
                    bytes.len(),
                    ids.len(),
                    want_old.as_ref().map(|f| f.bytes.len()),
                    want_new.as_ref().map(|f| f.bytes.len()),
                ));
            }
        }
    }
    Ok(())
}

/// Stronger check for completed applies: disk must EQUAL the new state.
fn assert_equals_new(target: &Path, new_m: &Model, poly: u64) {
    verify_consistent(target, &Model::default(), new_m, poly)
        .expect("completed apply must exactly equal the new state");
    assert_eq!(
        std::fs::read_dir(target).unwrap().count(),
        new_m.top_level_entries(),
        "no extras or leftovers allowed after completion"
    );
}

impl Model {
    fn top_level_entries(&self) -> usize {
        let files: Vec<&String> = self.files.keys().collect();
        let syms: Vec<&String> = self.symlinks.keys().collect();
        count_at_depth(&files)
            + count_at_depth(&syms)
            + self.dirs.iter().filter(|d| !d.contains('/')).count()
    }
}

fn count_at_depth(items: &[&String]) -> usize {
    items.iter().filter(|r| !r.contains('/')).count()
}

// ---------------------------------------------------------------------------
// The acceptance tests
// ---------------------------------------------------------------------------

/// Sanity A: without any kill, the apply completes and equals the new state
/// exactly. Also proves the harness plumbing (example bin, store reopen).
#[test]
fn kill_harness_no_kill_completes_to_exact_new_state() {
    let w = setup_world();
    let (old_m, new_m) = build_models();
    let tree = store_model(&w, &new_m);
    let target = w._dir.path().join("target");
    seed_target(&target, &old_m);

    let out = Command::new(apply_once_path())
        .arg("--store")
        .arg(&w.store_folder)
        .arg("--target")
        .arg(&target)
        .arg("--tree")
        .arg(hex(&tree))
        .arg("--fmk-hex")
        .arg(hex(&FMK))
        .output()
        .expect("failed to spawn apply_once");
    assert!(
        out.status.success(),
        "apply_once failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_equals_new(&target, &new_m, w.poly);
}

/// Sanity B: the verifier is not vacuous — a corrupted post-apply file must
/// be rejected, naming the path.
#[test]
fn kill_harness_verifier_rejects_torn_bytes() {
    let w = setup_world();
    let (old_m, new_m) = build_models();
    let tree = store_model(&w, &new_m);
    let target = w._dir.path().join("target");
    seed_target(&target, &old_m);

    let out = Command::new(apply_once_path())
        .arg("--store")
        .arg(&w.store_folder)
        .arg("--target")
        .arg(&target)
        .arg("--tree")
        .arg(hex(&tree))
        .arg("--fmk-hex")
        .arg(hex(&FMK))
        .output()
        .expect("failed to spawn apply_once");
    assert!(out.status.success());

    // Simulate a torn write: truncate one file mid-body.
    let victim = target.join("mod0.bin");
    let bytes = std::fs::read(&victim).unwrap();
    std::fs::write(&victim, &bytes[..bytes.len() / 2]).unwrap();
    let err = verify_consistent(&target, &old_m, &new_m, w.poly).unwrap_err();
    assert!(
        err.contains("mod0.bin"),
        "verifier must name the bad path: {err}"
    );
}

/// THE acceptance test: randomized SIGKILL at uniform offsets; every
/// iteration's seed and offset are logged; every surviving tree must be a
/// consistent old/new hybrid with no torn destination files and temps
/// confined to the documented pattern.
#[test]
#[cfg(unix)]
fn kill9_mid_apply_leaves_old_or_new_state_never_torn() {
    let w = setup_world();
    let (old_m, new_m) = build_models();
    let tree = store_model(&w, &new_m);

    println!("kill iterations: {KILL_ITERATIONS}");
    let mut offsets_hit: Vec<u64> = Vec::new();
    let mut i: usize = 0;
    let mut redraws: usize = 0;
    while i < KILL_ITERATIONS {
        let seed = SEED_BASE + i as u64;
        let mut rng = StdRng::seed_from_u64(seed);
        let mut offset_ms: u64 = rng.gen_range(15..=900);

        // One counted iteration = one kill that landed MID-APPLY. Pace is
        // per-mutation, so the child's total runtime is workload-bound: on
        // a fast or quiet host a large offset can outlive the whole apply
        // and the child exits 0 before SIGKILL lands (observed on Linux
        // CI). That proves nothing about torn safety, so shrink the offset
        // and redo this iteration; only kills that actually interrupted
        // are counted and verified below.
        let target = loop {
            let t = w._dir.path().join("target");
            seed_target(&t, &old_m);

            let mut child = Command::new(apply_once_path())
                .arg("--store")
                .arg(&w.store_folder)
                .arg("--target")
                .arg(&t)
                .arg("--tree")
                .arg(hex(&tree))
                .arg("--fmk-hex")
                .arg(hex(&FMK))
                .arg("--delay-ms")
                .arg("12")
                .spawn()
                .expect("spawn apply_once");

            std::thread::sleep(Duration::from_millis(offset_ms));
            let pid = child.id() as i32;
            let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
            assert_eq!(rc, 0, "kill failed");

            let status = child.wait().expect("wait failed");
            if status.success() {
                redraws += 1;
                assert!(
                    redraws <= KILL_ITERATIONS * 2,
                    "kills keep landing post-completion; lower the max offset or raise --delay-ms"
                );
                let _ = std::fs::remove_dir_all(&t);
                offset_ms = (offset_ms / 2).max(15);
                continue;
            }
            break t;
        };
        offsets_hit.push(offset_ms);

        verify_consistent(&target, &old_m, &new_m, w.poly).unwrap_or_else(|e| {
            panic!("iteration {i} (seed {seed}, offset {offset_ms}ms): inconsistent tree: {e}")
        });

        // Clean up this iteration's tree before the next.
        let _ = std::fs::remove_dir_all(&target);
        i += 1;
    }

    // Report coverage: offsets should span the window rather than cluster.
    let min = *offsets_hit.iter().min().unwrap();
    let max = *offsets_hit.iter().max().unwrap();
    println!(
        "offsets hit: min {min}ms max {max}ms across {KILL_ITERATIONS} runs ({redraws} redraws)"
    );
    println!("all offsets: {offsets_hit:?}");
    assert!(
        max > min + 200,
        "interruption points must vary meaningfully: {offsets_hit:?}"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
