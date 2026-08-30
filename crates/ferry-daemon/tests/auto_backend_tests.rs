#![allow(deprecated)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_daemon::ipc::spawn_ipc_server;
use ferry_daemon::state::DaemonState;
use ferry_daemon::ui::backend::{AutoBackend, InProcessAdapter};
use ferry_folder::folder::{create_folder, save_settings, Settings, SETTINGS_FORMAT_VERSION};
use ferry_ipc::backend::{SessionDomain, StatusDomain};
use ferry_ipc::client::DaemonClient;
use ferry_ipc::protocol::PinView;
use ferry_store::format::hex;
use ferry_sync::{EngineConfig, SyncEngine, TcpTransport};
use rand::rngs::StdRng;
use rand::SeedableRng;
use tempfile::TempDir;

struct TestRig {
    _temp_dir: TempDir,
    tree_dir: PathBuf,
    folder_id: [u8; 16],
    identity: DeviceIdentity,
    engine: SyncEngine,
}

impl TestRig {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("tempdir");
        let tree_dir = temp_dir.path().join("tree");
        std::fs::create_dir_all(&tree_dir).expect("create tree");

        let identity = DeviceIdentity::generate();
        let folder_id = [42u8; 16];
        let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::from_seed([42u8; 32]));

        // Initialize folder format
        let (store, _fmk) =
            create_folder(&tree_dir, &identity, folder_id, poly).expect("create folder");
        store.flush().expect("store flush");
        store.write_index_snapshot().expect("index snapshot");

        let settings = Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: hex(&folder_id),
            honor_gitignore: true,
            presets: Vec::new(),
            overrides: Vec::new(),
        };
        save_settings(&tree_dir, &settings).expect("save settings");

        let mut cfg = EngineConfig::default_for_test(42);
        cfg.tag = "test-node".to_string();
        cfg.store_dir.clone_from(&tree_dir);
        cfg.tree_dir.clone_from(&tree_dir);
        cfg.folder_id = folder_id;
        cfg.poly = ferry_store::chunker::ValidatedPoly::new(poly).expect("valid poly");
        cfg.pin_state_dir = Some(tree_dir.join(".ferry"));

        let mut engine = SyncEngine::with_store(cfg, Arc::new(TcpTransport), Arc::new(store))
            .expect("engine init");
        engine.set_identity(identity.clone());

        Self {
            _temp_dir: temp_dir,
            tree_dir,
            folder_id,
            identity,
            engine,
        }
    }
}

#[tokio::test]
async fn test_auto_backend_offline_then_online_then_offline_transition() {
    let rig = TestRig::new();
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&rig.tree_dir);

    let backend = AutoBackend::new(socket_path.clone())
        .with_fallback(rig.tree_dir.clone())
        .with_identity(rig.identity.clone());

    // Phase 1: Daemon is OFFLINE -> AutoBackend falls back to InProcessAdapter
    let offline_snap = backend
        .get_status()
        .await
        .expect("offline status via InProcessAdapter fallback");
    assert_eq!(offline_snap.folder, rig.tree_dir.display().to_string());
    assert_eq!(offline_snap.pin, PinView::none());
    assert_eq!(offline_snap.conflicts, 0);

    let offline_conflicts = backend
        .list_conflicts()
        .await
        .expect("offline list_conflicts");
    assert!(offline_conflicts.is_empty());

    // Phase 2: Start Daemon and IPC Server -> AutoBackend queries remote IPC
    let handle = rig.engine.start();
    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        rig.tree_dir.clone(),
        rig.tree_dir.clone(),
        rig.folder_id,
        rig.identity.clone(),
        tx,
    ));

    let ipc_server =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    // Give server a moment to bind and accept connections
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Status query now succeeds through IPC
    let online_snap = backend
        .get_status()
        .await
        .expect("online status via DaemonClient");
    assert_eq!(online_snap.folder, rig.tree_dir.display().to_string());

    // Start a pin over IPC through AutoBackend
    let pin_rec = backend
        .start_pin(vec!["src/main.rs".to_string()], None)
        .await
        .expect("start pin over IPC");
    assert_eq!(pin_rec.paths, vec!["src/main.rs"]);

    // Verify pin is reflected in daemon state snapshot
    let snap_pinned = backend.get_status().await.expect("pinned status");
    assert!(snap_pinned.pin.holding);
    assert_eq!(snap_pinned.pin.paths, vec!["src/main.rs"]);

    // Release pin over IPC
    let release_summary = backend.release_pin().await.expect("release pin over IPC");
    assert_eq!(release_summary.status, "release_pin");

    // Phase 3: Stop Daemon and IPC Server -> AutoBackend transitions back to InProcessAdapter
    ipc_server.shutdown();
    handle.shutdown();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let fallback_snap = backend
        .get_status()
        .await
        .expect("status after daemon stop via InProcessAdapter");
    assert_eq!(fallback_snap.folder, rig.tree_dir.display().to_string());

    let fallback_conflicts = backend
        .list_conflicts()
        .await
        .expect("conflicts after daemon stop");
    assert!(fallback_conflicts.is_empty());
}

#[tokio::test]
async fn test_daemon_client_direct() {
    let rig = TestRig::new();
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&rig.tree_dir);

    let ipc_client = DaemonClient::new(socket_path.clone());

    // Offline fails with the transport error code (fallback routing key)
    let err = ipc_client.get_status().await.unwrap_err();
    assert_eq!(err.code, "daemon-unreachable");

    // Start daemon
    let handle = rig.engine.start();
    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        rig.tree_dir.clone(),
        rig.tree_dir.clone(),
        rig.folder_id,
        rig.identity.clone(),
        tx,
    ));

    let ipc_server =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Status over IPC
    let snap = ipc_client.get_status().await.expect("get_status over IPC");
    assert_eq!(snap.folder, rig.tree_dir.display().to_string());

    // Trigger scan over IPC
    ipc_client
        .trigger_scan()
        .await
        .expect("trigger_scan over IPC");

    // List conflicts over IPC
    let conflicts = ipc_client
        .list_conflicts()
        .await
        .expect("list_conflicts over IPC");
    assert!(conflicts.is_empty());

    ipc_server.shutdown();
    handle.shutdown();
}
