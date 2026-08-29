//! T-18: Peer authorization policy integration tests.
//!
//! Acceptance criteria:
//! 1. The DEFAULT policy is an empty allow-list: it refuses every remote
//!    peer — nothing syncs to an unpaired device, and nothing falls back to
//!    trust on first use (ADR-0007).
//! 2. Opt-in TOFU (`PeerPolicy::TrustOnFirstUse`) pins the first peer's
//!    identity, refuses a DIFFERENT keypair with a typed error, and lets the
//!    same keypair proceed normally.
//! 3. Allow-list policies seed from `CONFIG_HEAD` on BOTH paired devices and
//!    deny unknown peers.
//! 4. Pinned identities survive engine restarts.

mod common;

use std::fs;
use std::sync::Arc;
use std::time::Duration;

use ferry_crypto::config_head::{write_config_head, WrappedKeyEntry};
use ferry_crypto::folder_key::WRAPPED_LEN;
use ferry_sync::engine::{device_identity_for_tag, PeerPolicy};
use ferry_sync::{BlobId, EngineConfig, SyncEngine, TcpTransport, DEFAULT_FOLDER_ID};

const SEED: u64 = 20260825;

#[test]
fn tofu_first_connect_pins_identity_and_second_different_keypair_is_refused() {
    let dir = tempfile::tempdir().unwrap();

    // Node A: listener in opt-in TOFU mode (ADR-0007: TOFU is never implied).
    let mut cfg_a = EngineConfig::default_for_test(SEED);
    cfg_a.tag = "tofu-a".into();
    cfg_a.store_dir = dir.path().join("a/store");
    cfg_a.tree_dir = dir.path().join("a/tree");
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    fs::create_dir_all(&cfg_a.tree_dir).unwrap();

    let mut engine_a = SyncEngine::new(cfg_a, Arc::new(TcpTransport)).unwrap();
    engine_a.set_peer_policy(PeerPolicy::TrustOnFirstUse);
    let addr_a = engine_a.listen_addr().unwrap();
    let handle_a = engine_a.start();

    // Initially no pinned peers on A.
    assert_eq!(handle_a.pinned_peers().unwrap(), Vec::<BlobId>::new());

    // Node B: connector with keypair derived from "tofu-b".
    let mut cfg_b = EngineConfig::default_for_test(SEED);
    cfg_b.tag = "tofu-b".into();
    cfg_b.store_dir = dir.path().join("b/store");
    cfg_b.tree_dir = dir.path().join("b/tree");
    cfg_b.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_b.tree_dir).unwrap();

    let id_b = *device_identity_for_tag("tofu-b").device_id();

    // Write a file on B and start B. B also opts into TOFU: the test
    // exercises the TOFU policy on both ends of the session.
    fs::write(dir.path().join("b/tree/file1.txt"), b"hello from b").unwrap();
    let mut engine_b = SyncEngine::new(cfg_b, Arc::new(TcpTransport)).unwrap();
    engine_b.set_peer_policy(PeerPolicy::TrustOnFirstUse);
    let handle_b = engine_b.start();

    // Wait for A and B to converge.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !(handle_a.agreed_id().is_some()
        && handle_a.agreed_id() == handle_b.agreed_id()
        && dir.path().join("a/tree/file1.txt").exists())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Node A and B failed to converge on first TOFU connect"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Node A has now pinned Node B's identity.
    let pinned = handle_a.pinned_peers().unwrap();
    assert_eq!(pinned, vec![id_b]);

    // Shut down Node B.
    handle_b.shutdown();

    let stats_before_c = handle_a.stats();

    // Node C: connector with a DIFFERENT keypair derived from "tofu-c".
    let mut cfg_c = EngineConfig::default_for_test(SEED);
    cfg_c.tag = "tofu-c".into();
    cfg_c.store_dir = dir.path().join("c/store");
    cfg_c.tree_dir = dir.path().join("c/tree");
    cfg_c.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_c.tree_dir).unwrap();

    fs::write(dir.path().join("c/tree/file_c.txt"), b"evil c bytes").unwrap();
    let engine_c = SyncEngine::new(cfg_c, Arc::new(TcpTransport)).unwrap();
    let handle_c = engine_c.start();

    // Wait and verify Node C fails to sync to Node A.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let s = handle_a.stats();
        if s.sessions_failed > stats_before_c.sessions_failed
            || s.rejected_items > stats_before_c.rejected_items
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !dir.path().join("a/tree/file_c.txt").exists(),
        "Node C's files must NOT be applied to Node A"
    );

    // Node A rejected Node C: sessions_failed and rejected_items incremented.
    let stats_after_c = handle_a.stats();
    assert!(
        stats_after_c.sessions_failed > stats_before_c.sessions_failed
            || stats_after_c.rejected_items > stats_before_c.rejected_items,
        "Node A must record rejection for unauthorized Node C"
    );

    handle_c.shutdown();

    // Now start Node B again with the original keypair; it must succeed.
    let mut cfg_b2 = EngineConfig::default_for_test(SEED);
    cfg_b2.tag = "tofu-b".into();
    cfg_b2.store_dir = dir.path().join("b/store");
    cfg_b2.tree_dir = dir.path().join("b/tree");
    cfg_b2.connect_to = Some(addr_a);

    fs::write(dir.path().join("b/tree/file2.txt"), b"second sync from b").unwrap();
    let mut engine_b2 = SyncEngine::new(cfg_b2, Arc::new(TcpTransport)).unwrap();
    engine_b2.set_peer_policy(PeerPolicy::TrustOnFirstUse);
    let handle_b2 = engine_b2.start();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !(handle_a.agreed_id() == handle_b2.agreed_id()
        && dir.path().join("a/tree/file2.txt").exists())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Node B failed to sync again with same pinned keypair"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(
        fs::read(dir.path().join("a/tree/file2.txt")).unwrap(),
        b"second sync from b"
    );

    handle_a.shutdown();
    handle_b2.shutdown();
}

#[test]
fn allow_list_policy_seeds_from_config_head_and_denies_unknown_peers() {
    let dir = tempfile::tempdir().unwrap();

    let id_a = *device_identity_for_tag("allow-a").device_id();
    let id_b = *device_identity_for_tag("allow-b").device_id();

    // Create CONFIG_HEAD for folder DEFAULT_FOLDER_ID with wrapped entries for A and B only.
    let entries = vec![
        WrappedKeyEntry::new(id_a, [1u8; WRAPPED_LEN]),
        WrappedKeyEntry::new(id_b, [2u8; WRAPPED_LEN]),
    ];
    let config_bytes = write_config_head(&DEFAULT_FOLDER_ID, &entries);

    let mut cfg_a = EngineConfig::default_for_test(SEED);
    cfg_a.tag = "allow-a".into();
    cfg_a.store_dir = dir.path().join("a/store");
    cfg_a.tree_dir = dir.path().join("a/tree");
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    fs::create_dir_all(&cfg_a.tree_dir).unwrap();

    let engine_a = SyncEngine::new(cfg_a, Arc::new(TcpTransport)).unwrap();
    // Write config to Node A's store directory: `<store_dir>/.ferry/config`.
    let dot_ferry_a = dir.path().join("a/store/.ferry");
    fs::write(dot_ferry_a.join("config"), &config_bytes).unwrap();

    let addr_a = engine_a.listen_addr().unwrap();
    let handle_a = engine_a.start();

    // Node B is in CONFIG_HEAD allow-list: connecting should succeed. The
    // pairing ritual writes the same CONFIG_HEAD on BOTH devices, so B
    // resolves its own allow-list from disk too (refuse is the default
    // otherwise, ADR-0007).
    let mut cfg_b = EngineConfig::default_for_test(SEED);
    cfg_b.tag = "allow-b".into();
    cfg_b.store_dir = dir.path().join("b/store");
    cfg_b.tree_dir = dir.path().join("b/tree");
    cfg_b.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_b.tree_dir).unwrap();
    let dot_ferry_b = dir.path().join("b/store/.ferry");
    fs::create_dir_all(&dot_ferry_b).unwrap();
    fs::write(dot_ferry_b.join("config"), &config_bytes).unwrap();

    fs::write(dir.path().join("b/tree/file_b.txt"), b"bytes from b").unwrap();
    let engine_b = SyncEngine::new(cfg_b, Arc::new(TcpTransport)).unwrap();
    let handle_b = engine_b.start();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !(handle_a.agreed_id().is_some()
        && handle_a.agreed_id() == handle_b.agreed_id()
        && dir.path().join("a/tree/file_b.txt").exists())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Allowed Node B failed to sync to Node A"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    handle_b.shutdown();

    // Node C is NOT in CONFIG_HEAD allow-list: connecting must be rejected.
    // C has no config of its own, so it also runs the refuse default — the
    // session is refused from both sides.
    let stats_before_c = handle_a.stats();
    let mut cfg_c = EngineConfig::default_for_test(SEED);
    cfg_c.tag = "allow-c".into();
    cfg_c.store_dir = dir.path().join("c/store");
    cfg_c.tree_dir = dir.path().join("c/tree");
    cfg_c.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_c.tree_dir).unwrap();

    fs::write(dir.path().join("c/tree/file_c.txt"), b"unauthorized c").unwrap();
    let engine_c = SyncEngine::new(cfg_c, Arc::new(TcpTransport)).unwrap();
    let handle_c = engine_c.start();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let s = handle_a.stats();
        if s.sessions_failed > stats_before_c.sessions_failed
            || s.rejected_items > stats_before_c.rejected_items
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !dir.path().join("a/tree/file_c.txt").exists(),
        "Unauthorized Node C must not sync to Node A"
    );

    let stats_after_c = handle_a.stats();
    assert!(
        stats_after_c.sessions_failed > stats_before_c.sessions_failed
            || stats_after_c.rejected_items > stats_before_c.rejected_items,
        "Node A must reject unauthorized Node C"
    );

    handle_c.shutdown();
    handle_a.shutdown();
}

#[test]
fn allow_list_pre_seed_skips_tofu_and_permits_multiple_allowed_peers() {
    let dir = tempfile::tempdir().unwrap();

    let id_a = *device_identity_for_tag("pre-a").device_id();
    let id_b = *device_identity_for_tag("pre-b").device_id();
    let id_d = *device_identity_for_tag("pre-d").device_id();

    let mut cfg_a = EngineConfig::default_for_test(SEED);
    cfg_a.tag = "pre-a".into();
    cfg_a.store_dir = dir.path().join("a/store");
    cfg_a.tree_dir = dir.path().join("a/tree");
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    fs::create_dir_all(&cfg_a.tree_dir).unwrap();

    let mut engine_a = SyncEngine::new(cfg_a, Arc::new(TcpTransport)).unwrap();
    // Pre-seed allow list with Node B and Node D.
    engine_a.set_peer_policy(PeerPolicy::from_allowed([id_b, id_d]));
    let addr_a = engine_a.listen_addr().unwrap();
    let handle_a = engine_a.start();

    // Node B connects and syncs. B's own allow-list admits A — with the
    // refuse default an unconfigured connector would refuse the session
    // from its side (ADR-0007).
    let mut cfg_b = EngineConfig::default_for_test(SEED);
    cfg_b.tag = "pre-b".into();
    cfg_b.store_dir = dir.path().join("b/store");
    cfg_b.tree_dir = dir.path().join("b/tree");
    cfg_b.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_b.tree_dir).unwrap();

    fs::write(dir.path().join("b/tree/b.txt"), b"from b").unwrap();
    let mut engine_b = SyncEngine::new(cfg_b, Arc::new(TcpTransport)).unwrap();
    engine_b.set_peer_policy(PeerPolicy::from_allowed([id_a]));
    let handle_b = engine_b.start();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !(handle_a.agreed_id().is_some()
        && handle_a.agreed_id() == handle_b.agreed_id()
        && dir.path().join("a/tree/b.txt").exists())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Node B failed to sync"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    handle_b.shutdown();

    // Node D connects and syncs (also in allow list).
    let mut cfg_d = EngineConfig::default_for_test(SEED);
    cfg_d.tag = "pre-d".into();
    cfg_d.store_dir = dir.path().join("d/store");
    cfg_d.tree_dir = dir.path().join("d/tree");
    cfg_d.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_d.tree_dir).unwrap();

    fs::write(dir.path().join("d/tree/d.txt"), b"from d").unwrap();
    let mut engine_d = SyncEngine::new(cfg_d, Arc::new(TcpTransport)).unwrap();
    engine_d.set_peer_policy(PeerPolicy::from_allowed([id_a]));
    let handle_d = engine_d.start();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !(handle_a.agreed_id() == handle_d.agreed_id()
        && dir.path().join("a/tree/d.txt").exists())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Node D failed to sync"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    handle_d.shutdown();

    // Node C is not in allow list: refused.
    let stats_before_c = handle_a.stats();
    let mut cfg_c = EngineConfig::default_for_test(SEED);
    cfg_c.tag = "pre-c".into();
    cfg_c.store_dir = dir.path().join("c/store");
    cfg_c.tree_dir = dir.path().join("c/tree");
    cfg_c.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_c.tree_dir).unwrap();

    fs::write(dir.path().join("c/tree/c.txt"), b"from c").unwrap();
    let engine_c = SyncEngine::new(cfg_c, Arc::new(TcpTransport)).unwrap();
    let handle_c = engine_c.start();

    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !dir.path().join("a/tree/c.txt").exists(),
        "Node C must be refused"
    );
    let stats_after_c = handle_a.stats();
    assert!(
        stats_after_c.sessions_failed > stats_before_c.sessions_failed
            || stats_after_c.rejected_items > stats_before_c.rejected_items
    );

    handle_c.shutdown();
    handle_a.shutdown();
}

#[test]
fn tofu_pinned_identity_survives_engine_restart() {
    let dir = tempfile::tempdir().unwrap();

    let store_dir_a = dir.path().join("a/store");
    let tree_dir_a = dir.path().join("a/tree");

    let mut cfg_a = EngineConfig::default_for_test(SEED);
    cfg_a.tag = "persist-a".into();
    cfg_a.store_dir = store_dir_a.clone();
    cfg_a.tree_dir = tree_dir_a.clone();
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    fs::create_dir_all(&cfg_a.tree_dir).unwrap();

    let mut engine_a = SyncEngine::new(cfg_a, Arc::new(TcpTransport)).unwrap();
    engine_a.set_peer_policy(PeerPolicy::TrustOnFirstUse);
    let addr_a = engine_a.listen_addr().unwrap();
    let handle_a = engine_a.start();

    let id_b = *device_identity_for_tag("persist-b").device_id();

    // Node B connects and syncs.
    let mut cfg_b = EngineConfig::default_for_test(SEED);
    cfg_b.tag = "persist-b".into();
    cfg_b.store_dir = dir.path().join("b/store");
    cfg_b.tree_dir = dir.path().join("b/tree");
    cfg_b.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_b.tree_dir).unwrap();

    fs::write(dir.path().join("b/tree/initial.txt"), b"first content").unwrap();
    let mut engine_b = SyncEngine::new(cfg_b, Arc::new(TcpTransport)).unwrap();
    engine_b.set_peer_policy(PeerPolicy::TrustOnFirstUse);
    let handle_b = engine_b.start();

    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !(handle_a.agreed_id().is_some()
        && handle_a.agreed_id() == handle_b.agreed_id()
        && dir.path().join("a/tree/initial.txt").exists())
    {
        assert!(std::time::Instant::now() < deadline, "Initial sync failed");
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(handle_a.pinned_peers().unwrap(), vec![id_b]);

    // Shut down both engines.
    handle_a.shutdown();
    handle_b.shutdown();

    // Restart Node A from the SAME store directory.
    let mut cfg_a_restarted = EngineConfig::default_for_test(SEED);
    cfg_a_restarted.tag = "persist-a".into();
    cfg_a_restarted.store_dir = store_dir_a;
    cfg_a_restarted.tree_dir = tree_dir_a;
    cfg_a_restarted.bind_addr = Some("127.0.0.1:0".parse().unwrap());

    let mut engine_a_restarted = SyncEngine::new(cfg_a_restarted, Arc::new(TcpTransport)).unwrap();
    engine_a_restarted.set_peer_policy(PeerPolicy::TrustOnFirstUse);
    let addr_a_restarted = engine_a_restarted.listen_addr().unwrap();
    let handle_a_restarted = engine_a_restarted.start();

    // Check that pinned peers are present immediately upon reopening.
    assert_eq!(handle_a_restarted.pinned_peers().unwrap(), vec![id_b]);

    // Try connecting Node C (different key) to restarted Node A.
    let stats_before_c = handle_a_restarted.stats();
    let mut cfg_c = EngineConfig::default_for_test(SEED);
    cfg_c.tag = "persist-c".into();
    cfg_c.store_dir = dir.path().join("c/store");
    cfg_c.tree_dir = dir.path().join("c/tree");
    cfg_c.connect_to = Some(addr_a_restarted);
    fs::create_dir_all(&cfg_c.tree_dir).unwrap();

    fs::write(dir.path().join("c/tree/bad.txt"), b"evil").unwrap();
    let engine_c = SyncEngine::new(cfg_c, Arc::new(TcpTransport)).unwrap();
    let handle_c = engine_c.start();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        let s = handle_a_restarted.stats();
        if s.sessions_failed > stats_before_c.sessions_failed
            || s.rejected_items > stats_before_c.rejected_items
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !dir.path().join("a/tree/bad.txt").exists(),
        "Node C must be refused after restart"
    );
    let stats_after_c = handle_a_restarted.stats();
    assert!(
        stats_after_c.sessions_failed > stats_before_c.sessions_failed
            || stats_after_c.rejected_items > stats_before_c.rejected_items
    );
    handle_c.shutdown();

    // Node B connects to restarted Node A: must succeed.
    let mut cfg_b_restarted = EngineConfig::default_for_test(SEED);
    cfg_b_restarted.tag = "persist-b".into();
    cfg_b_restarted.store_dir = dir.path().join("b/store");
    cfg_b_restarted.tree_dir = dir.path().join("b/tree");
    cfg_b_restarted.connect_to = Some(addr_a_restarted);

    fs::write(dir.path().join("b/tree/second.txt"), b"second content").unwrap();
    let mut engine_b_restarted = SyncEngine::new(cfg_b_restarted, Arc::new(TcpTransport)).unwrap();
    engine_b_restarted.set_peer_policy(PeerPolicy::TrustOnFirstUse);
    let handle_b_restarted = engine_b_restarted.start();

    // Heaviest wait in this file: B reconnects to a RESTARTED A (TOFU
    // re-pin, fresh session, full sync). Under loaded CI runners two
    // sightings exceeded 15s (flakes.md 2026-08-25 and run 32903163863);
    // give it the same 30s class as the iroh acceptance budgets.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !(handle_a_restarted.agreed_id() == handle_b_restarted.agreed_id()
        && dir.path().join("a/tree/second.txt").exists())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "Node B failed to sync after restart"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(
        fs::read(dir.path().join("a/tree/second.txt")).unwrap(),
        b"second content"
    );

    handle_a_restarted.shutdown();
    handle_b_restarted.shutdown();
}

#[test]
fn empty_allow_list_refuses_unpaired_devices() {
    let dir = tempfile::tempdir().unwrap();

    let id_a = *device_identity_for_tag("refuse-a").device_id();

    // Node A: listener with the DEFAULT policy — an empty allow-list. No
    // device is paired on A, so every remote peer is refused; there is no
    // trust-on-first-use fallback (ADR-0007, glossary explicit-pairing rule).
    let mut cfg_a = EngineConfig::default_for_test(SEED);
    cfg_a.tag = "refuse-a".into();
    cfg_a.store_dir = dir.path().join("a/store");
    cfg_a.tree_dir = dir.path().join("a/tree");
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    fs::create_dir_all(&cfg_a.tree_dir).unwrap();

    let engine_a = SyncEngine::new(cfg_a, Arc::new(TcpTransport)).unwrap();
    let addr_a = engine_a.listen_addr().unwrap();
    let handle_a = engine_a.start();

    // Node B is willing to talk to A, but A has not paired it: the session
    // must be refused, not pinned.
    let mut cfg_b = EngineConfig::default_for_test(SEED);
    cfg_b.tag = "refuse-b".into();
    cfg_b.store_dir = dir.path().join("b/store");
    cfg_b.tree_dir = dir.path().join("b/tree");
    cfg_b.connect_to = Some(addr_a);
    fs::create_dir_all(&cfg_b.tree_dir).unwrap();

    fs::write(dir.path().join("b/tree/unpaired.txt"), b"from unpaired b").unwrap();
    let mut engine_b = SyncEngine::new(cfg_b, Arc::new(TcpTransport)).unwrap();
    engine_b.set_peer_policy(PeerPolicy::from_allowed([id_a]));
    let handle_b = engine_b.start();

    // Wait for A to register the refused session.
    let stats_before = handle_a.stats();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let s = handle_a.stats();
        if s.sessions_failed > stats_before.sessions_failed
            || s.rejected_items > stats_before.rejected_items
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        !dir.path().join("a/tree/unpaired.txt").exists(),
        "unpaired Node B's files must NOT be applied to Node A"
    );
    assert!(
        handle_a.pinned_peers().unwrap().is_empty(),
        "the refuse default must not record a TOFU pin for the unpaired peer"
    );
    let stats_after = handle_a.stats();
    assert!(
        stats_after.sessions_failed > stats_before.sessions_failed
            || stats_after.rejected_items > stats_before.rejected_items,
        "Node A must refuse the unpaired Node B"
    );

    handle_a.shutdown();
    handle_b.shutdown();
}
