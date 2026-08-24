//! Integration tests: real notify watcher on the host OS, real debounce
//! worker, poll fallback, and audit timer. Deterministic policy/walker logic
//! lives in unit tests; these cover the wiring end to end.
//!
//! Timing discipline: every wait is a deadline-bounded retry loop (5 s cap),
//! never a bare sleep assertion, so slow machines pass without inflating
//! happy-path latency.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ferry_scan::config::ScanConfig;
use ferry_scan::engine::{ScanEngine, StoreHandle};
use ferry_scan::policy::WatchSignal;
use ferry_store::crypto::PassthroughCipher;
use ferry_store::diff::{diff_manifests, ChangeSet};
use ferry_store::snapshot::snapshot_dir;
use ferry_store::store::Store;
use rand::SeedableRng;

fn fmk() -> [u8; 32] {
    core::array::from_fn(|i| i as u8)
}

fn poly() -> u64 {
    // A fixed-seed polynomial keeps this test independent of global RNG
    // state; irreducibility correctness is ferry-store's own concern.
    ferry_store::chunker::generate_polynomial(&mut rand::rngs::StdRng::seed_from_u64(42))
}

fn handle_for(store: &Arc<Store>) -> StoreHandle {
    StoreHandle {
        store: store.clone(),
        poly: poly(),
        folder_id: [5; 16],
        device_id: [6; 32],
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
}

fn env(name: &str) -> (Env, Arc<Store>) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join(name);
    std::fs::create_dir_all(&root).unwrap();
    let store_root = tmp.path().join("store-root");
    std::fs::create_dir_all(&store_root).unwrap();
    let store = Arc::new(Store::create(&store_root, fmk(), Box::new(PassthroughCipher)).unwrap());
    (Env { _tmp: tmp, root }, store)
}

fn fast_cfg() -> ScanConfig {
    ScanConfig {
        quiet_window: Duration::from_millis(150),
        audit_interval: Duration::from_hours(1),
        poll_interval: Duration::from_millis(100),
    }
}

/// Wait until `f` returns Some; panics after `deadline`.
fn wait_until<T>(deadline: Instant, mut f: impl FnMut() -> Option<T>) -> T {
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "condition not met within deadline"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn scratch_root(store: &Store, root: &std::path::Path) -> ferry_store::BlobId {
    let id = ferry_store::snapshot::SnapshotIdentity {
        folder_id: [5; 16],
        device_id: [6; 32],
        parent_manifest_id: [0; 32],
        created_sec: 1,
        created_nsec: 0,
    };
    snapshot_dir(store, poly(), root, &id).unwrap().root_tree_id
}

#[test]
fn watched_rename_of_subdir_recovers_correct_state() {
    let (env, store) = env("rename-me");
    write(&env.root.join("sub/inner.txt"), b"before");
    write(&env.root.join("top.txt"), b"top");

    let engine = ScanEngine::watch(env.root.clone(), handle_for(&store)).unwrap();
    let baseline = engine.current().unwrap();

    // Rename a watched subdir with contents behind the running watcher.
    std::fs::rename(env.root.join("sub"), env.root.join("renamed")).unwrap();
    write(&env.root.join("renamed/after.txt"), b"added during rename");

    let deadline = Instant::now() + Duration::from_secs(5);
    let cur = wait_until(deadline, || {
        let c = engine.current()?;
        (c.root_tree_id != baseline.root_tree_id).then_some(c)
    });

    assert_eq!(
        cur.root_tree_id,
        scratch_root(&store, &env.root),
        "post-rename manifest equals from-scratch"
    );
    let cs = diff_manifests(&store, &baseline.manifest, &cur.manifest).unwrap();
    assert!(
        !cs.added.is_empty() && cs.removed.iter().any(|r| r.path.join("/").contains("sub")),
        "rename must surface as removal of old paths + additions: {cs:?}"
    );
}

#[test]
fn overflow_injection_triggers_full_rescan_and_repairs_arbitrary_drift() {
    let (env, store) = env("overflow");
    write(&env.root.join("a.txt"), b"one");
    write(&env.root.join("d/b.txt"), b"two");
    let engine = ScanEngine::watch_with(
        env.root.clone(),
        handle_for(&store),
        fast_cfg(),
        Arc::new(ferry_scan::NoIgnores),
    )
    .unwrap();
    let baseline = engine.current().unwrap();

    // Drift that produces NO watcher events we can rely on in CI timing:
    // delete + create + content change all at once.
    std::fs::remove_file(env.root.join("a.txt")).unwrap();
    write(&env.root.join("d/b.txt"), b"two-changed");
    write(&env.root.join("new.txt"), b"brand new");

    // Simulate kernel queue overflow exactly as the platform glue would.
    engine.debug_inject_signal(WatchSignal::Overflow {
        reason: "test-injected IN_Q_OVERFLOW".into(),
    });

    let deadline = Instant::now() + Duration::from_secs(5);
    let cur = wait_until(deadline, || {
        let c = engine.current()?;
        (c.trigger == ferry_scan::Trigger::OverflowRecovery).then_some(c)
    });
    assert_eq!(cur.root_tree_id, scratch_root(&store, &env.root));
    assert_ne!(cur.manifest_id, baseline.manifest_id);
}

#[test]
fn poll_fallback_converges_when_watch_is_unavailable() {
    let (env, store) = env("polled");
    write(&env.root.join("watched-ok/x.txt"), b"native");
    write(&env.root.join("unwatchable/y.txt"), b"poll me");
    let engine = ScanEngine::watch_with(
        env.root.clone(),
        handle_for(&store),
        fast_cfg(),
        Arc::new(ferry_scan::NoIgnores),
    )
    .unwrap();

    // Declare the subtree unwatchable through the normal signal path
    // (what ENOSPC during watch registration produces).
    engine.debug_inject_signal(WatchSignal::Unwatchable {
        subtree: vec!["unwatchable".to_string()],
        reason: "test-injected ENOSPC".into(),
    });

    // Mutate ONLY the polled subtree. On a live watcher the native events
    // may win the race (platform-dependent), so the honest assertion here is
    // CONVERGENCE: whichever path fires, the manifest must match disk. The
    // poller's own mismatch detection is covered deterministically by
    // engine unit tests over stat_sweep.
    let before = engine.current().unwrap();
    write(&env.root.join("unwatchable/y.txt"), b"polled change");

    let deadline = Instant::now() + Duration::from_secs(5);
    let cur = wait_until(deadline, || {
        let c = engine.current()?;
        (c.manifest_id != before.manifest_id && c.stats.bytes_chunked > 0).then_some(c)
    });
    assert_eq!(cur.root_tree_id, scratch_root(&store, &env.root));
}

#[test]
fn audit_timer_detects_silent_same_length_rewrite() {
    let (env, store) = env("audit");
    write(&env.root.join("vault.bin"), &[7u8; 256]);
    let cfg = ScanConfig {
        quiet_window: Duration::from_millis(50),
        audit_interval: Duration::from_millis(250),
        poll_interval: Duration::from_millis(100),
    };
    let engine = ScanEngine::watch_with(
        env.root.clone(),
        handle_for(&store),
        cfg,
        Arc::new(ferry_scan::NoIgnores),
    )
    .unwrap();
    let baseline = engine.current().unwrap();

    // Same length, mtime restored: invisible to stat-based short-circuits.
    std::fs::write(env.root.join("vault.bin"), [9u8; 256]).unwrap();
    set_mtime(&env.root.join("vault.bin"));

    let deadline = Instant::now() + Duration::from_secs(5);
    // On a live watcher the write itself produces events (platform race);
    // what we assert is that the audit TIMER runs full-hash passes and that
    // drift is repaired. The "incremental misses it" half is proven
    // deterministically in walk.rs unit tests.
    let cur = wait_until(deadline, || {
        let c = engine.current()?;
        (c.root_tree_id != baseline.root_tree_id).then_some(c)
    });
    wait_until(deadline, || {
        engine.last_pass().and_then(|(t, s)| {
            (t == ferry_scan::Trigger::Audit && s.files_rehashed >= 1).then_some(())
        })
    });
    assert_eq!(cur.root_tree_id, scratch_root(&store, &env.root));
    let cs = diff_manifests(&store, &baseline.manifest, &cur.manifest).unwrap();
    let mut touched = cs.content_modified.len() + cs.added.len() + cs.removed.len();
    touched += cs.metadata_modified.len();
    assert!(touched >= 1, "{cs:?}");
}

#[test]
fn burst_of_writes_coalesces_and_subscribers_are_notified() {
    let (env, store) = env("burst");
    write(&env.root.join("seed.txt"), b"seed");
    let engine = ScanEngine::watch_with(
        env.root.clone(),
        handle_for(&store),
        fast_cfg(),
        Arc::new(ferry_scan::NoIgnores),
    )
    .unwrap();
    let rx = engine.subscribe();

    for i in 0..30 {
        write(
            &env.root.join(format!("burst{i}.txt")),
            format!("payload{i}").as_bytes(),
        );
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    let updates = wait_until(deadline, || {
        let mut count = 0;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                ferry_scan::ScanEvent::Updated(c) => {
                    count += 1;
                    if c.stats.files >= 31 {
                        return Some(count);
                    }
                }
                ferry_scan::ScanEvent::Failed(m) => panic!("scan failed: {m}"),
            }
        }
        None
    });
    assert_eq!(
        updates, 1,
        "a single burst must coalesce into ONE update event"
    );

    // And the final state matches disk truth.
    let cur = engine.current().unwrap();
    assert_eq!(cur.root_tree_id, scratch_root(&store, &env.root));

    // Zero-change rescan hashes nothing.
    let run = engine.scan_once().unwrap();
    assert_eq!(run.stats.bytes_chunked, 0);
}

#[test]
fn incremental_pass_matches_scratch_after_event_driven_mutations() {
    let (env, store) = env("events");
    for i in 0..10 {
        write(
            &env.root.join(format!("f{i}.txt")),
            format!("v{i}").as_bytes(),
        );
    }
    write(&env.root.join("nested/deep.txt"), b"deep");
    let engine = ScanEngine::watch_with(
        env.root.clone(),
        handle_for(&store),
        fast_cfg(),
        Arc::new(ferry_scan::NoIgnores),
    )
    .unwrap();
    let baseline = engine.current().unwrap();

    std::fs::remove_file(env.root.join("f3.txt")).unwrap();
    write(&env.root.join("f4.txt"), b"v4 rewritten");
    std::fs::create_dir(env.root.join("fresh")).unwrap();
    write(&env.root.join("fresh/g.txt"), b"g");

    let deadline = Instant::now() + Duration::from_secs(5);
    let cur = wait_until(deadline, || {
        let c = engine.current()?;
        (c.root_tree_id != baseline.root_tree_id && c.stats.files_rehashed >= 2).then_some(c)
    });
    assert_eq!(cur.root_tree_id, scratch_root(&store, &env.root));
    let cs = diff_manifests(&store, &baseline.manifest, &cur.manifest).unwrap();
    let empty = ChangeSet::default();
    assert_ne!(cs, empty);
}

// --- tiny local helpers (tests/ cannot use crate-internal testutil) ---

fn write(path: &std::path::Path, bytes: &[u8]) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn set_mtime(path: &std::path::Path) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_times(
        std::fs::FileTimes::new()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
    )
    .unwrap();
}
