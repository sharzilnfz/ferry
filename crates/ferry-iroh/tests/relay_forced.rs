//! THE local acceptance proof for T-009 (the verbatim ticket acceptance —
//! two machines behind separate home NATs — is MANUAL-UNRUN; this file is
//! its local stand-in, docs/nat-validation.md is its runbook).
//!
//! What is proven here, honestly:
//!
//! 1. **Relay-forced convergence**: with `force_relay` (iroh's
//!    `clear_ip_transports`) BOTH endpoints can reach each other ONLY via a
//!    running ferry-relay, and the full ferry-sync engine still converges
//!    end-to-end through it. This is the same shape as "two NATs": every
//!    byte of the sync transits the relay.
//! 2. **Plaintext absence at the relay**: every line the relay logs plus
//!    its structured connection ledger are scanned for the transferred
//!    plaintext markers and must contain NONE. The relay's metadata
//!    surface (endpoint public keys, connects) IS expected — its absence
//!    would mean the scan was vacuous.
//! 3. **Direct upgrade in normal mode**: without forcing, iroh's own
//!    negotiation moves the selected path from relay to direct; observed
//!    via PathObservation, again with zero engine awareness.

use std::io::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use std::time::Instant;

use ferry_iroh::{IrohConfig, IrohTransport};
use ferry_sync::format::hex;
use ferry_sync::{EngineConfig, SyncEngine};
use rand::rngs::StdRng;
use rand::SeedableRng as _;

/// One shared capture buffer per test binary: tracing allows exactly one
/// global subscriber, so all relay-side log lines land here.
static CAPTURE: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

fn relay_log_capture() -> &'static Arc<Mutex<Vec<u8>>> {
    CAPTURE.get_or_init(|| {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        // First-and-only global install; tracing permits one subscriber per
        // process, and one test binary is one process.
        let _ = ferry_relay::install_capturing_subscriber(Arc::clone(&buffer));
        buffer
    })
}

fn scan_relay_side(needle: &str) -> usize {
    let logs = relay_log_capture().lock().unwrap();
    let hay = String::from_utf8_lossy(&logs);
    hay.matches(needle).count()
}

struct PairFixture {
    _dir: tempfile::TempDir,
    a: (ferry_sync::EngineHandle, IrohTransport),
    b: (ferry_sync::EngineHandle, IrohTransport),
}

fn start_pair(force_relay: bool, relay_url: Option<String>, name: &str) -> PairFixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(0xBEEF));

    let mk_transport = |seed_byte: u8| {
        let mut seed = [0u8; 32];
        seed[0] = seed_byte;
        seed[31] = 0x77;
        let mut cfg = IrohConfig::builder().secret(seed);
        if let Some(url) = &relay_url {
            cfg = cfg.relays(ferry_iroh::RelaySetting::Custom(vec![url.clone()]));
        }
        if force_relay {
            cfg = cfg.force_relay(true);
        }
        let cfg = cfg.dial_timeout(Duration::from_secs(15)).build();
        IrohTransport::new(cfg).expect("transport")
    };

    let t_a = mk_transport(0x11);
    let t_b = mk_transport(0x22);

    std::fs::create_dir_all(dir.path().join("a/tree")).unwrap();
    std::fs::create_dir_all(dir.path().join("b/tree")).unwrap();

    let mut cfg_a = EngineConfig::default_for_test(poly);
    cfg_a.tag = format!("{name}-a");
    cfg_a.store_dir = dir.path().join("a/store");
    cfg_a.tree_dir = dir.path().join("a/tree");
    cfg_a.quiet = true;
    cfg_a.bind_addr = Some("127.0.0.1:0".parse().unwrap());
    let engine_a = SyncEngine::new(cfg_a, Arc::new(t_a.clone())).expect("engine A");
    let addr = engine_a.listen_addr().expect("A bound an alias");

    let mut cfg_b = EngineConfig::default_for_test(poly);
    cfg_b.tag = format!("{name}-b");
    cfg_b.store_dir = dir.path().join("b/store");
    cfg_b.tree_dir = dir.path().join("b/tree");
    cfg_b.quiet = true;
    cfg_b.connect_to = Some(addr);

    let engine_a_started = engine_a.start();
    let engine_b = SyncEngine::new(cfg_b, Arc::new(t_b.clone())).expect("engine B");
    let engine_b_started = engine_b.start();

    PairFixture {
        _dir: dir,
        a: (engine_a_started, t_a),
        b: (engine_b_started, t_b),
    }
}

const MARKER_NEEDLE: &str = "FERRY-PLAINTEXT-MARKER";

/// Write distinctive plaintext into A's tree: file CONTENTS carrying the
/// marker needle, plus filenames that also carry it. Everything here ends
/// up on the wire in M0 (pass-through cipher) but must never appear at the
/// relay, whose view is endpoint-to-endpoint QUIC ciphertext.
fn plant_markers(tree_a: &std::path::Path) -> Vec<String> {
    let mut rels = Vec::new();
    for i in 0..12 {
        let rel = format!("secret-notes-{i}.txt");
        let p = tree_a.join(&rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(&p).unwrap();
        for line in 0..40 {
            writeln!(
                f,
                "{MARKER_NEEDLE}-{i:02}-line{line:03} top secret env API_KEY=sk-live-{i}{line}"
            )
            .unwrap();
        }
        rels.push(rel);
    }
    rels
}

fn wait_converged(pair: &PairFixture, budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        assert!(
            Instant::now() < deadline,
            "no convergence within {budget:?}"
        );
        let (ha, hb) = (&pair.a.0, &pair.b.0);
        if let (Some(x), Some(y)) = (ha.agreed_id(), hb.agreed_id()) {
            if x == y && x != [0u8; 32] && ha.root_id() == hb.root_id() && ha.root_id().is_some() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[test]
fn forced_relay_mode_converges_and_relay_sees_no_plaintext() {
    let relay = ferry_relay::spawn_sync(ferry_relay::RelayOptions::new(
        "127.0.0.1:0".parse().unwrap(),
    ))
    .expect("local relay spawns");
    let _capture = relay_log_capture(); // ensure subscriber exists from the start

    let pair = start_pair(
        true, /* FORCE */
        Some(relay.url().to_string()),
        "forced",
    );

    let tree_a = pair._dir.path().join("a/tree");
    let tree_b = pair._dir.path().join("b/tree");
    let planted = plant_markers(&tree_a);

    wait_converged(&pair, Duration::from_secs(90));
    for rel in &planted {
        assert!(
            tree_b.join(rel).is_file(),
            "marked file {rel} did not arrive"
        );
    }

    // Give iroh's path sampler one last beat, then stop the engines so
    // observations are final before asserting.
    std::thread::sleep(Duration::from_millis(300));
    pair.a.0.shutdown();
    pair.b.0.shutdown();

    // --- Path evidence: traffic rode the relay and NEVER went direct. ---
    let obs_b = pair
        .b
        .1
        .path_observation(&pair.a.1.endpoint_id())
        .expect("B observed paths to A");
    assert!(
        obs_b
            .selected_relay_seen
            .load(std::sync::atomic::Ordering::SeqCst),
        "no relay-selected path observed: data did not demonstrably transit the relay"
    );
    assert!(
        !obs_b
            .selected_ip_seen
            .load(std::sync::atomic::Ordering::SeqCst),
        "an IP path was selected despite force_relay config"
    );

    // --- Relay-side ledger: metadata present (non-vacuous), plaintext absent. ---
    let entries = relay.ledger().entries();
    let ids: Vec<String> = entries
        .iter()
        .map(|e| match e {
            ferry_relay::LedgerEntry::Connected {
                endpoint_id_hex, ..
            } => endpoint_id_hex.clone(),
            ferry_relay::LedgerEntry::Disconnected { endpoint_id_hex } => endpoint_id_hex.clone(),
        })
        .collect();
    assert!(
        ids.contains(&hex(&pair.a.1.endpoint_id())),
        "relay never saw node A connect: {:?}",
        ids
    );
    assert!(
        ids.contains(&hex(&pair.b.1.endpoint_id())),
        "relay never saw node B connect: {:?}",
        ids
    );

    // The actual plaintext-absence assertion, over BOTH surfaces:
    assert_eq!(
        scan_relay_side(MARKER_NEEDLE),
        0,
        "PLAINTEXT LEAKED into relay logs"
    );
    assert_eq!(scan_relay_side("API_KEY"), 0, "secret-shaped text leaked");
    assert_eq!(scan_relay_side("secret-notes"), 0, "filenames leaked");
}

#[test]
fn normal_mode_upgrades_to_direct_per_iroh_negotiation() {
    let relay = ferry_relay::spawn_sync(ferry_relay::RelayOptions::new(
        "127.0.0.1:0".parse().unwrap(),
    ))
    .expect("local relay spawns");

    // NOT forced: relays available, direct transports intact.
    let pair = start_pair(false, Some(relay.url().to_string()), "normal");

    let tree_a = pair._dir.path().join("a/tree");
    let tree_b = pair._dir.path().join("b/tree");
    std::fs::write(tree_a.join("hello.txt"), b"normal mode hello").unwrap();

    wait_converged(&pair, Duration::from_secs(90));
    // Agreement ids settle before the materializer's rename lands on B's
    // disk under loaded-runner scheduling; poll briefly rather than race.
    let landed_by = Instant::now() + Duration::from_secs(30);
    loop {
        match std::fs::read(tree_b.join("hello.txt")) {
            Ok(bytes) => {
                assert_eq!(bytes, b"normal mode hello");
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                assert!(
                    Instant::now() < landed_by,
                    "hello.txt never landed on b after convergence"
                );
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => panic!("read failed: {e}"),
        }
    }

    // Watch the negotiation settle on a direct path (same-host here; the
    // mechanism — hole punch / local addresses — is iroh's job either way).
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let obs = pair
            .b
            .1
            .path_observation(&pair.a.1.endpoint_id())
            .expect("paths observed");
        if obs
            .selected_ip_seen
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "iroh negotiation never upgraded to a direct path"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    pair.a.0.shutdown();
    pair.b.0.shutdown();
}
