












mod common;

use std::time::{Duration, Instant};

use rand::SeedableRng;

use ferry_sync_engine::pin::{release_peer, HeldLedger, PinRecord, PinStore, PIN_FORMAT_VERSION};
use ferry_store::agreement::AgreementLedger;
use ferry_store::snapshot::{snapshot_dir, SnapshotIdentity};
use ferry_sync::format::{hex, unhex};
use ferry_sync::{engine, DEFAULT_FOLDER_ID};

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
    let b_dev = *engine::device_identity_for_tag(TAG_B).device_id();
    let read = |root: &std::path::Path, rel: &str| std::fs::read_to_string(root.join(rel));

    
    std::fs::create_dir_all(fx.tree_a().join("docs")).unwrap();
    std::fs::write(fx.tree_a().join("notes.txt"), b"v1").unwrap();
    std::fs::write(fx.tree_a().join("docs/other.txt"), b"d1").unwrap();
    wait_until("baseline convergence", || {
        read(&fx.tree_b(), "notes.txt").is_ok_and(|v| v == "v1")
            && read(&fx.tree_b(), "docs/other.txt").is_ok_and(|v| v == "d1")
    });
    wait_until("agreement recorded vs B", || {
        AgreementLedger::new(&a_ferry)
            .get(&DEFAULT_FOLDER_ID, &b_dev)
            .unwrap_or(None)
            .is_some()
    });
    let agreed = AgreementLedger::new(&a_ferry)
        .get(&DEFAULT_FOLDER_ID, &b_dev)
        .unwrap()
        .unwrap();
    let mut bases = std::collections::BTreeMap::new();
    bases.insert(b_hex.clone(), hex(&agreed.manifest_id));

    
    let (sec, nsec) = ferry_platform_time();
    PinStore::new(&a_ferry)
        .start(&PinRecord {
            format_version: PIN_FORMAT_VERSION,
            device_id: hex(engine::device_identity_for_tag(TAG_A).device_id()),
            pid: std::process::id(),
            started_sec: sec,
            started_nsec: nsec,
            expires_sec: None,
            paths: vec!["notes.txt".to_string()],
            released: false,
            base_agreements: bases.clone(),
            proc_start_token: None, 
        })
        .expect("pin starts");
    let rec = PinStore::new(&a_ferry).load().unwrap().expect("record");
    assert!(rec.holding(), "the engine's own pin must count as active");

    
    std::fs::write(fx.tree_b().join("notes.txt"), b"v2").unwrap();
    std::fs::write(fx.tree_b().join("docs/other.txt"), b"d2").unwrap();

    
    wait_until("unpinned doc flows while pinned", || {
        read(&fx.tree_a(), "docs/other.txt").is_ok_and(|v| v == "d2")
    });
    
    assert_eq!(
        read(&fx.tree_a(), "notes.txt").unwrap(),
        "v1",
        "pinned peer edit must not touch the tree"
    );

    
    
    let ledger = HeldLedger::new(&a_ferry);
    
    
    wait_until("held decision surfaced for notes.txt", || {
        ledger
            .load_peer(&b_hex)
            .unwrap()
            .iter()
            .any(|e| e.path == "notes.txt")
    });
    let entries = ledger.load_peer(&b_hex).unwrap();
    let notes = entries
        .iter()
        .find(|e| e.path == "notes.txt")
        .expect("held decision surfaced for notes.txt");
    assert_eq!(notes.device_id, b_hex);
    assert_eq!(notes.decision, "remote_apply");
    assert!(!notes.chunks.is_empty(), "held bytes ride the fetch");
    let remote_man_id = unhex::<32>(&notes.remote_manifest_id).unwrap();
    let opened_a = ferry_folder::folder::open_folder(
        &fx._dir.path().join("a/store"),
        &engine::device_identity_for_tag(TAG_A),
    )
    .unwrap();
    assert!(
        opened_a
            .store
            .get(ferry_store::format::BlobKind::Manifest, &remote_man_id)
            .is_ok(),
        "held remote manifest must be stored in the content-addressed blob store during hold"
    );

    
    
    
    let mut seen = std::collections::BTreeSet::new();
    for e in ledger.load_peer(&b_hex).unwrap() {
        assert!(
            seen.insert((e.path.clone(), e.remote_manifest_id.clone())),
            "duplicate held line for {} in {}",
            e.path,
            e.remote_manifest_id
        );
    }

    
    PinStore::new(&a_ferry).mark_released().unwrap();

    
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

    
    
    
    
    let opened = ferry_folder::folder::open_folder(
        &fx._dir.path().join("a/store"),
        &engine::device_identity_for_tag(TAG_A),
    )
    .unwrap();
    let store = opened.store;
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
    let base_hex = bases.get(&b_hex).expect("pin-start agreement captured");
    let base_bytes = store
        .get(
            ferry_store::format::BlobKind::Manifest,
            &unhex::<32>(base_hex).unwrap(),
        )
        .expect("base manifest blob present");
    let base = ferry_store::manifest::parse_manifest(&base_bytes).unwrap();
    let (rel_sec, rel_nsec) = ferry_platform_time();
    let released = release_peer(
        &store,
        &fx.tree_a(),
        &a_ferry,
        &snap.manifest,
        &b_hex,
        Some(&base),
        (rel_sec, rel_nsec),
    )
    .expect("release converges");
    assert!(
        released.held_paths.contains(&"notes.txt".to_string()),
        "release reconciles the held path"
    );
    assert!(ledger.clear_peer(&b_hex).unwrap());

    assert_eq!(
        read(&fx.tree_a(), "notes.txt").unwrap(),
        "v2",
        "release materializes the winner: only the remote side changed"
    );

    fx.b.shutdown();
    fx.a.shutdown();
}


fn ferry_platform_time() -> (i64, u32) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}
