//! T-06 acceptance: session pinning enforced ON THE ENGINE PATH (not the
//! CLI exchange loop).
//!
//! Two real engines over loopback TCP; node A runs with
//! `pin_state_dir` configured (what the daemon binary wires). While A's
//! pin scopes `notes.txt`, B's edit to that path is withheld from A's
//! tree at the shared execution boundary and surfaced in
//! `.ferry/held/<peer>.jsonl`, while unpinned changes still land. After
//! the pin ends, the ordinary engine flow carries new peer changes again,
//! and the offline release planner (`plan_release` + `execute`, the exact
//! machinery `ferry pin release` drives) recovers the held version from
//! the bytes fetched during the hold — no peer required.

mod common;

use std::time::{Duration, Instant};

use rand::SeedableRng;

use ferry_pin::{HeldLedger, PinRecord, PinStore};
use ferry_store::crypto::{PassthroughCipher, KEY_LEN};
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};
use ferry_store::store::Store;
use ferry_sync::format::{hex, unhex};
use ferry_sync::{engine, AgreementStore};

use common::{timeout_from_env, EngineFixture};

const SEED: u64 = 77;
const TAG_A: &str = "pin-a";
const TAG_B: &str = "pin-b";

fn wait_until(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + timeout_from_env();
    while !cond() {
        assert!(Instant::now() <= deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn engine_holds_pinned_peer_changes_and_release_recovers_them() {
    let fx = EngineFixture::start_with_cfg_a("pin", SEED, |cfg| {
        cfg.pin_state_dir = Some(cfg.store_dir.join(".ferry"));
    });
    let a_ferry = fx._dir.path().join("a/store/.ferry");
    let b_hex = hex(engine::device_identity_for_tag(TAG_B).device_id());
    let read = |root: &std::path::Path, rel: &str| std::fs::read_to_string(root.join(rel));

    // --- phase 1: converge both engines on baseline content --------------
    std::fs::create_dir_all(fx.tree_a().join("docs")).unwrap();
    std::fs::write(fx.tree_a().join("notes.txt"), b"v1").unwrap();
    std::fs::write(fx.tree_a().join("docs/other.txt"), b"d1").unwrap();
    wait_until("baseline convergence", || {
        read(&fx.tree_b(), "notes.txt").is_ok_and(|v| v == "v1")
            && read(&fx.tree_b(), "docs/other.txt").is_ok_and(|v| v == "d1")
    });
    wait_until("agreement recorded vs B", || {
        AgreementStore::new(&a_ferry)
            .load(&b_hex)
            .unwrap_or(None)
            .is_some()
    });
    let (agreed, _) = AgreementStore::new(&a_ferry).load(&b_hex).unwrap().unwrap();
    let mut bases = std::collections::BTreeMap::new();
    bases.insert(b_hex.clone(), hex(&agreed.manifest_id));

    // --- phase 2: A pins notes.txt ---------------------------------------
    let (sec, nsec) = ferry_platform_time();
    PinStore::new(&a_ferry)
        .start(&PinRecord {
            format_version: ferry_pin::PIN_FORMAT_VERSION,
            device_id: hex(engine::device_identity_for_tag(TAG_A).device_id()),
            pid: std::process::id(),
            started_sec: sec,
            started_nsec: nsec,
            paths: vec!["notes.txt".to_string()],
            released: false,
            base_agreements: bases.clone(),
            proc_start_token: None, // start() stamps this writer itself
        })
        .expect("pin starts");
    let rec = PinStore::new(&a_ferry).load().unwrap().expect("record");
    assert!(rec.holding(), "the engine's own pin must count as active");

    // --- phase 3: B changes the pinned AND an unpinned path ---------------
    std::fs::write(fx.tree_b().join("notes.txt"), b"v2").unwrap();
    std::fs::write(fx.tree_b().join("docs/other.txt"), b"d2").unwrap();

    // Unpinned change lands through the enforced boundary...
    wait_until("unpinned doc flows while pinned", || {
        read(&fx.tree_a(), "docs/other.txt").is_ok_and(|v| v == "d2")
    });
    // ...but the pinned path is HELD: A's tree keeps living its version.
    assert_eq!(
        read(&fx.tree_a(), "notes.txt").unwrap(),
        "v1",
        "pinned peer edit must not touch the tree"
    );

    // Surfaced: ledgered exactly where release looks, with the held
    // version's chunk refs (bytes were fetched during the hold).
    let ledger = HeldLedger::new(&a_ferry);
    let entries = ledger.load_peer(&b_hex).unwrap();
    let notes = entries
        .iter()
        .find(|e| e.path == "notes.txt")
        .expect("held decision surfaced for notes.txt");
    assert_eq!(notes.device_id, b_hex);
    assert_eq!(notes.decision, "remote_apply");
    assert!(!notes.chunks.is_empty(), "held bytes ride the fetch");
    assert_eq!(unhex::<32>(&notes.remote_manifest_id).map(|_| ()), Some(()));

    // Storage-efficiency directive: long pins span many poll ticks, so
    // identical rounds must append NOTHING. Whatever else happens under
    // load, one held line per (path, remote manifest) is the contract.
    let mut seen = std::collections::BTreeSet::new();
    for e in ledger.load_peer(&b_hex).unwrap() {
        assert!(
            seen.insert((e.path.clone(), e.remote_manifest_id.clone())),
            "duplicate held line for {} in {}",
            e.path,
            e.remote_manifest_id
        );
    }

    // --- phase 4: pin ends -------------------------------------------------
    PinStore::new(&a_ferry).mark_released().unwrap();

    // Ordinary flow resumes: fresh peer edits apply again without ceremony.
    std::fs::write(fx.tree_b().join("docs/other.txt"), b"d4").unwrap();
    {
        let deadline = Instant::now() + timeout_from_env();
        while !matches!(read(&fx.tree_a(), "docs/other.txt"), Ok(v) if v == "d4") {
            if Instant::now() > deadline {
                let dump = |root: &std::path::Path| -> Vec<String> {
                    std::fs::read_dir(root)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter_map(|e| e.file_name().into_string().ok())
                        .collect()
                };
                panic!(
                    "timed out waiting for post-release flow resumes\n\
                     agreed_a={:?} agreed_b={:?}\n\
                     tree_a={:?} tree_b={:?}\n\
                     pin={:?}\n\
                     held_dir={:?}",
                    fx.a.agreed_id(),
                    fx.b.agreed_id(),
                    std::fs::read_to_string(fx.tree_a().join("docs/other.txt")),
                    std::fs::read_to_string(fx.tree_b().join("docs/other.txt")),
                    std::fs::read_to_string(a_ferry.join("pin-state.json")),
                    dump(&a_ferry.join("held")),
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // And the HELD version recovers OFFLINE through the documented release
    // path (plan_release + engine execute — what `ferry pin release` runs),
    // reconciling against the last-agreed base frozen at pin start.
    let store = Store::open(
        &fx._dir.path().join("a/store"),
        [0u8; KEY_LEN],
        Box::new(PassthroughCipher),
    )
    .unwrap();
    let poly =
        ferry_store::chunker::ValidatedPoly::generate(&mut rand::rngs::StdRng::seed_from_u64(SEED));
    let snap = snapshot_dir(
        &store,
        poly,
        &fx.tree_a(),
        &SnapshotIdentity {
            folder_id: [7; 16],
            device_id: *engine::device_identity_for_tag(TAG_A).device_id(),
            parent_manifest_id: [0; 32],
            created_sec: 1_900_000_000,
            created_nsec: 0,
        },
    )
    .unwrap();
    let plans = ferry_pin::plan_release(&store, &snap.manifest, &bases, &ledger).unwrap();
    let plan = plans
        .iter()
        .find(|p| p.device_id == b_hex)
        .expect("release plan rebuilt for the pinning peer");
    assert!(plan.held_paths.contains(&"notes.txt".to_string()));
    let (rel_sec, rel_nsec) = ferry_platform_time();
    let _stats = ferry_sync_engine::execute(
        &store,
        &fx.tree_a(),
        &plan.plan,
        Some(&a_ferry),
        (rel_sec, rel_nsec),
    )
    .expect("release plan executes");
    assert!(ledger.clear_peer(&b_hex).unwrap());

    assert_eq!(
        read(&fx.tree_a(), "notes.txt").unwrap(),
        "v2",
        "release materializes the winner: only the remote side changed"
    );

    fx.b.shutdown();
    fx.a.shutdown();
}

/// Local wall clock for stamps (tests do not need timefmt formatting).
fn ferry_platform_time() -> (i64, u32) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}
