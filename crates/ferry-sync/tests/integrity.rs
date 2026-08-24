//! Transfer integrity: one byte flipped in transit inside a pack ITEM must
//! be caught by the receiver's name check (BLAKE3(ciphertext) == requested
//! pack name), the session must fail WITHOUT recording agreement, and the
//! engine must retry and still converge.

mod common;

use std::time::Duration;

use common::{timeout_from_env, CorruptingTransport, EngineFixture, TreeBuilder};

#[test]
fn corrupted_transfer_is_rejected_and_retry_still_converges() {
    let mut fx = EngineFixture::start("integ", 555);

    let mut tb = TreeBuilder::new(fx.tree_a(), 31337);
    for i in 0..12 {
        tb.write_random(&format!("blobby/f{i}.bin"), 16384);
    }

    // Rebuild B so it dials through the corrupting hook: the first ITEM
    // frame B receives is flipped in flight. The hook wraps whatever
    // transport the suite selected (FERRY_SYNC_E2E_TRANSPORT), so integrity
    // semantics are proven per-transport.
    let hook = CorruptingTransport::new(common::default_transport());
    let b = fx.replace_b(hook.clone());

    let deadline = std::time::Instant::now() + timeout_from_env();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no convergence after corruption: A={:?} B={:?}",
            fx.a.stats(),
            b.stats()
        );
        // The corruption must actually have fired and been rejected...
        if hook.fired()
            && b.stats().rejected_items >= 1
            && b.stats().sessions_failed >= 1
            // ...and the system must STILL converge.
            && fx.converged()
            && common::trees_identical(&fx.tree_a(), &fx.tree_b())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(fx.a.agreed_id(), b.agreed_id());
}

#[test]
fn pack_name_verification_rejects_tampered_bytes_directly() {
    // Unit-level: ingest path refuses bytes whose BLAKE3 != claimed name,
    // without touching disk state.
    use ferry_sync::{IngestError, SyncEngine};

    let dir = tempfile::tempdir().unwrap();
    let cfg = ferry_sync::EngineConfig {
        store_dir: dir.path().join("s"),
        tree_dir: dir.path().join("t"),
        ..ferry_sync::EngineConfig::default_for_test(42)
    };
    std::fs::create_dir_all(&cfg.tree_dir).unwrap();

    let real = vec![1u8, 2, 3];
    let fake_name = [9u8; 32];
    let err = SyncEngine::ingest_pack_bytes_for_test(&cfg, &fake_name, &real).unwrap_err();
    assert!(matches!(err, IngestError::NameMismatch { .. }), "{err}");

    // The bad pack never landed on disk.
    let packs = std::fs::read_dir(cfg.store_dir.join(".ferry/packs"))
        .unwrap()
        .count();
    assert_eq!(packs, 0);
}
