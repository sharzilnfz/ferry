use std::sync::Arc;
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_daemon::ipc::spawn_ipc_server;
use ferry_daemon::state::DaemonState;
use ferry_daemon::ui::backend::{AutoBackend, DaemonIpcAdapter, InProcessAdapter};
use ferry_folder::folder::{create_folder, open_folder, save_settings, Settings, SETTINGS_FORMAT_VERSION};
use ferry_ipc::backend::UiBackend;
use ferry_store::format::hex;
use ferry_sync::{EngineConfig, SyncEngine, TcpTransport};
use rand::rngs::StdRng;
use rand::SeedableRng;
use tempfile::TempDir;

#[tokio::test]
async fn test_in_band_pairing_in_process_adapters() {
    let dir_alice = TempDir::new().unwrap();
    let dir_bob = TempDir::new().unwrap();

    let identity_alice = DeviceIdentity::generate();
    let identity_bob = DeviceIdentity::generate();

    // Alice initializes folder
    let folder_id = [77u8; 16];
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::from_seed([77u8; 32]));
    let (store_a, _) = create_folder(dir_alice.path(), &identity_alice, folder_id, poly).unwrap();
    store_a.flush().unwrap();
    store_a.write_index_snapshot().unwrap();

    let settings_a = Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: hex(&folder_id),
        honor_gitignore: true,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    save_settings(dir_alice.path(), &settings_a).unwrap();

    let adapter_alice = InProcessAdapter::new(dir_alice.path().to_path_buf())
        .with_identity(identity_alice.clone());

    let adapter_bob = InProcessAdapter::new(dir_bob.path().to_path_buf())
        .with_identity(identity_bob.clone());

    // Alice creates pairing session (host)
    let host_sess = adapter_alice
        .create_pairing_session(None)
        .await
        .expect("alice creates pairing session");

    assert_eq!(host_sess.role, "host");
    assert_eq!(host_sess.status, "advertising");
    let code_words: Vec<&str> = host_sess.code.split('-').collect();
    assert_eq!(code_words.len(), 6);

    // Bob joins pairing session with 6-word code (joiner)
    let join_res = adapter_bob
        .join_pairing_session(host_sess.code.clone(), Some(dir_bob.path().to_path_buf()))
        .await
        .expect("bob joins pairing session");

    assert_eq!(join_res.status, "completed");
    assert_eq!(join_res.folder_id, hex(&folder_id));

    // Verify Bob's store was initialized with Alice's FMK
    let opened_bob = open_folder(dir_bob.path(), &identity_bob).unwrap();
    assert_eq!(opened_bob.folder_id, folder_id);
    assert_eq!(opened_bob.poly, poly);

    let opened_alice = open_folder(dir_alice.path(), &identity_alice).unwrap();
    let fmk_a = ferry_folder::folder::unwrap_own_fmk(&opened_alice, &identity_alice).unwrap();
    let fmk_b = ferry_folder::folder::unwrap_own_fmk(&opened_bob, &identity_bob).unwrap();
    assert_eq!(fmk_a, fmk_b);

    // Verify NO .ferry-pair files were created on either host or joiner
    assert!(!dir_alice.path().join(".ferry").join("offer.ferry-pair").exists());
    assert!(!dir_alice.path().join(".ferry").join("response.ferry-pair").exists());
    assert!(!dir_alice.path().join(".ferry").join("grant.ferry-pair").exists());
    assert!(!dir_bob.path().join(".ferry").join("offer.ferry-pair").exists());
    assert!(!dir_bob.path().join(".ferry").join("response.ferry-pair").exists());
    assert!(!dir_bob.path().join(".ferry").join("grant.ferry-pair").exists());
}

#[tokio::test]
async fn test_in_band_pairing_over_daemon_ipc() {
    let dir_alice = TempDir::new().unwrap();
    let dir_bob = TempDir::new().unwrap();

    let identity_alice = DeviceIdentity::generate();
    let identity_bob = DeviceIdentity::generate();

    // Alice initializes folder and starts sync engine + IPC server
    let folder_id = [88u8; 16];
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::from_seed([88u8; 32]));
    let (store_a, _) = create_folder(dir_alice.path(), &identity_alice, folder_id, poly).unwrap();
    store_a.flush().unwrap();
    store_a.write_index_snapshot().unwrap();

    let settings_a = Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: hex(&folder_id),
        honor_gitignore: true,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    save_settings(dir_alice.path(), &settings_a).unwrap();

    let mut cfg = EngineConfig::default_for_test(88);
    cfg.tag = "alice-node".to_string();
    cfg.store_dir = dir_alice.path().to_path_buf();
    cfg.tree_dir = dir_alice.path().to_path_buf();
    cfg.folder_id = folder_id;
    cfg.poly = ferry_store::chunker::ValidatedPoly::new(poly).unwrap();
    cfg.pin_state_dir = Some(dir_alice.path().join(".ferry"));

    let mut engine_alice = SyncEngine::new(cfg, Arc::new(TcpTransport)).unwrap();
    engine_alice.set_identity(identity_alice.clone());
    let handle_alice = engine_alice.start();

    let (tx_a, _) = tokio::sync::broadcast::channel(128);
    let daemon_state_alice = Arc::new(DaemonState::new(
        handle_alice.clone(),
        dir_alice.path().to_path_buf(),
        dir_alice.path().to_path_buf(),
        folder_id,
        identity_alice.clone(),
        tx_a,
    ));

    let socket_path_alice = ferry_ipc::paths::socket_path_for_dir(dir_alice.path());
    let server_alice = spawn_ipc_server(socket_path_alice.clone(), Arc::clone(&daemon_state_alice)).unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let ipc_alice = DaemonIpcAdapter::new(socket_path_alice);
    let auto_bob = AutoBackend::new(ferry_ipc::paths::socket_path_for_dir(dir_bob.path()))
        .with_fallback(dir_bob.path().to_path_buf())
        .with_identity(identity_bob.clone());

    // Alice creates pairing session over IPC
    let host_sess = ipc_alice
        .create_pairing_session(None)
        .await
        .expect("alice creates pairing session over IPC");
    assert_eq!(host_sess.role, "host");
    assert_eq!(host_sess.code.split('-').count(), 6);

    // Bob joins pairing session
    let join_res = auto_bob
        .join_pairing_session(host_sess.code.clone(), Some(dir_bob.path().to_path_buf()))
        .await
        .expect("bob joins pairing session");

    assert_eq!(join_res.status, "completed");
    assert_eq!(join_res.folder_id, hex(&folder_id));

    // Verify Bob's store was initialized
    let opened_alice = open_folder(dir_alice.path(), &identity_alice).unwrap();
    let opened_bob = open_folder(dir_bob.path(), &identity_bob).unwrap();
    assert_eq!(opened_bob.folder_id, folder_id);
    assert_eq!(opened_bob.poly, poly);

    let fmk_a = ferry_folder::folder::unwrap_own_fmk(&opened_alice, &identity_alice).unwrap();
    let fmk_b = ferry_folder::folder::unwrap_own_fmk(&opened_bob, &identity_bob).unwrap();
    assert_eq!(fmk_a, fmk_b);

    server_alice.shutdown();
    handle_alice.shutdown();
}

#[tokio::test]
async fn test_in_band_pairing_invalid_code_rejected() {
    let dir_bob = TempDir::new().unwrap();
    let identity_bob = DeviceIdentity::generate();
    let adapter_bob = InProcessAdapter::new(dir_bob.path().to_path_buf())
        .with_identity(identity_bob);

    // Unknown mnemonic has no active host route
    let err = adapter_bob
        .join_pairing_session(
            "abandon-abandon-abandon-abandon-abandon-abandon".to_string(),
            Some(dir_bob.path().to_path_buf()),
        )
        .await
        .unwrap_err();

    assert_eq!(err.code, "not-found");
}
