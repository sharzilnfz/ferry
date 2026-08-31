use std::io::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use std::time::Instant;

use ferry_iroh::{IrohConfig, IrohTransport};
use ferry_sync::format::hex;
use ferry_sync::{EngineConfig, SyncEngine};
use rand::rngs::StdRng;
use rand::SeedableRng as _;

static CAPTURE: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

fn relay_log_capture() -> &'static Arc<Mutex<Vec<u8>>> {
    CAPTURE.get_or_init(|| {
        let buffer = Arc::new(Mutex::new(Vec::new()));

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

    let shared = ferry_iroh::RouteTable::new();
    let mk_transport = |seed_byte: u8| {
        let mut seed = [0u8; 32];
        seed[0] = seed_byte;
        seed[31] = 0x77;
        let cfg = IrohConfig {
            secret: Some(seed),
            routes: Some(shared.clone()),
            relays: if let Some(url) = &relay_url {
                ferry_iroh::RelaySetting::Custom(vec![url.clone()])
            } else {
                ferry_iroh::RelaySetting::Disabled
            },
            force_relay,
            dial_timeout: Duration::from_secs(15),
            ..Default::default()
        };
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

    let mut cfg_b = EngineConfig::default_for_test(poly);
    cfg_b.tag = format!("{name}-b");
    cfg_b.store_dir = dir.path().join("b/store");
    cfg_b.tree_dir = dir.path().join("b/tree");
    cfg_b.quiet = true;

    let id_a = ferry_sync::engine::device_identity_for_tag(&cfg_a.tag);
    let id_b = ferry_sync::engine::device_identity_for_tag(&cfg_b.tag);

    let (store_a, fmk) =
        ferry_folder::folder::create_folder(&cfg_a.store_dir, &id_a, cfg_a.folder_id, poly)
            .expect("create folder a");
    ferry_folder::folder::save_settings(
        &cfg_a.store_dir,
        &ferry_folder::folder::Settings {
            format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
            folder_id: ferry_sync::format::hex(&cfg_a.folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )
    .unwrap();
    store_a.flush().unwrap();
    store_a.write_index_snapshot().unwrap();

    let store_b =
        ferry_folder::folder::adopt_folder(&cfg_b.store_dir, &id_b, cfg_b.folder_id, &fmk, poly)
            .expect("adopt folder b");
    ferry_folder::folder::save_settings(
        &cfg_b.store_dir,
        &ferry_folder::folder::Settings {
            format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
            folder_id: ferry_sync::format::hex(&cfg_b.folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )
    .unwrap();
    store_b.flush().unwrap();
    store_b.write_index_snapshot().unwrap();

    let mut engine_a =
        SyncEngine::with_store(cfg_a, Arc::new(t_a.clone()), Arc::new(store_a)).expect("engine A");
    engine_a.set_peer_policy(ferry_sync::PeerPolicy::from_allowed([*id_b.public()]));
    let addr = engine_a.listen_addr().expect("A bound an alias");

    cfg_b.connect_to = Some(addr);

    let mut engine_b =
        SyncEngine::with_store(cfg_b, Arc::new(t_b.clone()), Arc::new(store_b)).expect("engine B");
    engine_b.set_peer_policy(ferry_sync::PeerPolicy::from_allowed([*id_a.public()]));
    let engine_a_started = engine_a.start();
    let engine_b_started = engine_b.start();

    PairFixture {
        _dir: dir,
        a: (engine_a_started, t_a),
        b: (engine_b_started, t_b),
    }
}

const MARKER_NEEDLE: &str = "FERRY-PLAINTEXT-MARKER";

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

fn wait_markers_landed(tree_b: &std::path::Path, rels: &[String], budget: Duration) {
    let deadline = Instant::now() + budget;
    loop {
        let missing: Vec<_> = rels.iter().filter(|r| !tree_b.join(r).is_file()).collect();
        if missing.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "markers never landed on b after convergence: {missing:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn forced_relay_mode_converges_and_relay_sees_no_plaintext() {
    let relay = ferry_relay::spawn_sync(ferry_relay::RelayOptions::new(
        "127.0.0.1:0".parse().unwrap(),
    ))
    .expect("local relay spawns");
    let _capture = relay_log_capture();

    let pair = start_pair(true, Some(relay.url().to_string()), "forced");

    let tree_a = pair._dir.path().join("a/tree");
    let tree_b = pair._dir.path().join("b/tree");
    let planted = plant_markers(&tree_a);

    wait_converged(&pair, Duration::from_secs(90));
    wait_markers_landed(&tree_b, &planted, Duration::from_secs(30));

    std::thread::sleep(Duration::from_millis(300));
    pair.a.0.shutdown();
    pair.b.0.shutdown();

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
        "relay never saw node A connect: {ids:?}"
    );
    assert!(
        ids.contains(&hex(&pair.b.1.endpoint_id())),
        "relay never saw node B connect: {ids:?}"
    );

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

    let pair = start_pair(false, Some(relay.url().to_string()), "normal");

    let tree_a = pair._dir.path().join("a/tree");
    let tree_b = pair._dir.path().join("b/tree");
    std::fs::write(tree_a.join("hello.txt"), b"normal mode hello").unwrap();

    wait_converged(&pair, Duration::from_secs(90));

    let landed_by = Instant::now() + Duration::from_secs(90);
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
