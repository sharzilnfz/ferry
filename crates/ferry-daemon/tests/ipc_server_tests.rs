use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_daemon::ipc::{handle_client_connection, spawn_ipc_server};
use ferry_daemon::state::DaemonState;
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, PinView};
use ferry_ipc::{create_in_memory_pair, IpcClient};
use ferry_sync::{EngineConfig, SyncEngine, TcpTransport};
use tempfile::TempDir;

struct TestRig {
    _temp_dir: TempDir,
    store_dir: PathBuf,
    tree_dir: PathBuf,
    folder_id: [u8; 16],
    identity: DeviceIdentity,
    engine: SyncEngine,
}

impl TestRig {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("tempdir");
        let store_dir = temp_dir.path().join("store");
        let tree_dir = temp_dir.path().join("tree");
        std::fs::create_dir_all(&store_dir).expect("create store");
        std::fs::create_dir_all(&tree_dir).expect("create tree");

        let identity = DeviceIdentity::generate();
        let folder_id = [42u8; 16];

        let mut cfg = EngineConfig::default_for_test(42);
        cfg.tag = "test-node".to_string();
        cfg.store_dir.clone_from(&store_dir);
        cfg.tree_dir.clone_from(&tree_dir);
        cfg.folder_id = folder_id;
        cfg.pin_state_dir = Some(store_dir.join(".ferry"));

        let mut engine = SyncEngine::new(cfg, Arc::new(TcpTransport)).expect("engine init");
        engine.set_identity(identity.clone());

        Self {
            _temp_dir: temp_dir,
            store_dir,
            tree_dir,
            folder_id,
            identity,
            engine,
        }
    }
}

#[tokio::test]
async fn test_daemon_ipc_server_startup_snapshot_and_cleanup() {
    let rig = TestRig::new();
    let handle = rig.engine.start();

    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        rig.store_dir.clone(),
        rig.tree_dir.clone(),
        rig.folder_id,
        rig.identity.clone(),
        tx,
    ));

    let socket_path = ferry_ipc::paths::socket_path_for_dir(&rig.store_dir);
    let ipc_handle =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    // 1. Verify socket exists
    #[cfg(unix)]
    assert!(socket_path.exists(), "socket file must exist after startup");

    // 2. Connect client
    let mut client = IpcClient::connect(&socket_path)
        .await
        .expect("client connect");

    // 3. Receive initial snapshot
    let first_msg = client
        .recv_message()
        .await
        .expect("recv msg")
        .expect("msg exists");

    match first_msg {
        DaemonMessage::Snapshot(snap) => {
            assert_eq!(snap.folder, rig.tree_dir.display().to_string());
            assert_eq!(snap.pin, PinView::none());
            assert_eq!(snap.conflicts, 0);
        }
        other => panic!("expected DaemonMessage::Snapshot, got {other:?}"),
    }

    // Disconnect client
    drop(client);

    // 4. Shutdown IPC server and verify cleanup
    ipc_handle.shutdown();

    #[cfg(unix)]
    assert!(
        !socket_path.exists(),
        "socket file must be removed after server shutdown"
    );

    handle.shutdown();
}

#[tokio::test]
async fn test_daemon_ipc_command_dispatch() {
    let rig = TestRig::new();
    let handle = rig.engine.start();

    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        rig.store_dir.clone(),
        rig.tree_dir.clone(),
        rig.folder_id,
        rig.identity.clone(),
        tx,
    ));

    let socket_path = ferry_ipc::paths::socket_path_for_dir(&rig.store_dir);
    let ipc_handle =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    let mut client = IpcClient::connect(&socket_path)
        .await
        .expect("client connect");

    // Consume initial snapshot
    let init_snap = client.recv_message().await.unwrap().unwrap();
    assert!(matches!(init_snap, DaemonMessage::Snapshot(_)));

    // Test Ping -> Pong
    client.send_command(&ClientCommand::Ping).await.unwrap();
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Pong => break,
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    // Test GetStatus -> Snapshot
    client
        .send_command(&ClientCommand::GetStatus)
        .await
        .unwrap();
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Snapshot(_) => break,
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    // Test StartPin -> Ack
    client
        .send_command(&ClientCommand::StartPin {
            paths: vec!["src/main.rs".to_string()],
            duration_hours: None,
        })
        .await
        .unwrap();

    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, .. } => {
                assert_eq!(command, "start_pin");
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("expected Ack start_pin, got {other:?}"),
        }
    }

    // Verify pin is active in subsequent snapshot
    client
        .send_command(&ClientCommand::GetStatus)
        .await
        .unwrap();
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Snapshot(snap) => {
                assert!(snap.pin.holding);
                assert_eq!(snap.pin.paths, vec!["src/main.rs"]);
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("expected Snapshot, got {other:?}"),
        }
    }

    // Test TriggerScan -> Ack
    client
        .send_command(&ClientCommand::TriggerScan)
        .await
        .unwrap();
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, .. } => {
                assert_eq!(command, "trigger_scan");
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("expected Ack trigger_scan, got {other:?}"),
        }
    }

    // Test ListConflicts -> Ack
    client
        .send_command(&ClientCommand::ListConflicts)
        .await
        .unwrap();
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, .. } => {
                assert_eq!(command, "list_conflicts");
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("expected Ack list_conflicts, got {other:?}"),
        }
    }

    // Test ReleasePin -> Ack
    client
        .send_command(&ClientCommand::ReleasePin)
        .await
        .unwrap();
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, .. } => {
                assert_eq!(command, "release_pin");
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("expected Ack release_pin, got {other:?}"),
        }
    }

    ipc_handle.shutdown();
    handle.shutdown();
}

#[tokio::test]
async fn test_daemon_broadcast_on_state_change() {
    let rig = TestRig::new();
    let handle = rig.engine.start();

    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        rig.store_dir.clone(),
        rig.tree_dir.clone(),
        rig.folder_id,
        rig.identity.clone(),
        tx,
    ));

    let socket_path = ferry_ipc::paths::socket_path_for_dir(&rig.store_dir);
    let ipc_handle =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    let mut client_a = IpcClient::connect(&socket_path)
        .await
        .expect("client a connect");
    let mut client_b = IpcClient::connect(&socket_path)
        .await
        .expect("client b connect");

    // Both receive initial snapshot
    let snap_a = client_a.recv_message().await.unwrap().unwrap();
    let snap_b = client_b.recv_message().await.unwrap().unwrap();
    assert!(matches!(snap_a, DaemonMessage::Snapshot(_)));
    assert!(matches!(snap_b, DaemonMessage::Snapshot(_)));

    // Client A starts a pin -> should broadcast StateChanged to all clients
    client_a
        .send_command(&ClientCommand::StartPin {
            paths: vec!["file.txt".to_string()],
            duration_hours: None,
        })
        .await
        .unwrap();

    // Client A receives its command Ack (and may also receive broadcasted StateChanged)
    let msg1 = client_a.recv_message().await.unwrap().unwrap();
    let is_ack_first = matches!(msg1, DaemonMessage::Ack { .. });
    if !is_ack_first {
        assert!(matches!(msg1, DaemonMessage::StateChanged { .. }));
        let msg2 = client_a.recv_message().await.unwrap().unwrap();
        assert!(matches!(msg2, DaemonMessage::Ack { .. }));
    }

    // Client B receives the broadcasted StateChanged event
    let event = client_b.recv_message().await.unwrap().unwrap();
    match event {
        DaemonMessage::StateChanged { .. } => {}
        other => panic!("expected DaemonMessage::StateChanged, got {other:?}"),
    }

    ipc_handle.shutdown();
    handle.shutdown();
}

#[tokio::test]
async fn test_daemon_broadcast_on_conflict_recorded() {
    let rig = TestRig::new();
    let handle = rig.engine.start();

    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        rig.store_dir.clone(),
        rig.tree_dir.clone(),
        rig.folder_id,
        rig.identity.clone(),
        tx,
    ));

    let socket_path = ferry_ipc::paths::socket_path_for_dir(&rig.store_dir);
    let ipc_handle =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    let mut client = IpcClient::connect(&socket_path)
        .await
        .expect("client connect");

    // Consume initial snapshot
    let _ = client.recv_message().await.unwrap().unwrap();

    // Append a conflict to .ferry/conflicts.jsonl
    let entry = ferry_sync_engine::ConflictEntry {
        ts: "2026-08-26T12:00:00Z".to_string(),
        folder_id: "0102030405060708090a0b0c0d0e0f10".to_string(),
        path: "important.txt".to_string(),
        kind: "both_changed".to_string(),
        winner: ferry_sync_engine::DeviceStamp {
            device: "aaaa".to_string(),
            mtime_sec: Some(123),
            mtime_nsec: Some(0),
        },
        loser: ferry_sync_engine::DeviceStamp {
            device: "bbbb".to_string(),
            mtime_sec: Some(120),
            mtime_nsec: Some(0),
        },
        quarantined_as: Some("important.txt.ferry-conflict.bbbb-20260826-120000".to_string()),
    };

    ferry_sync_engine::append_entries(&rig.store_dir.join(".ferry"), &[entry]).unwrap();

    // The background watcher will detect the new conflict entry and broadcast ConflictRecorded
    let mut received_conflict = false;
    for _ in 0..20 {
        match tokio::time::timeout(Duration::from_millis(200), client.recv_message()).await {
            Ok(Ok(Some(DaemonMessage::ConflictRecorded {
                path,
                conflict_path,
                ..
            }))) => {
                assert_eq!(path, "important.txt");
                assert_eq!(
                    conflict_path,
                    "important.txt.ferry-conflict.bbbb-20260826-120000"
                );
                received_conflict = true;
                break;
            }
            Ok(Ok(Some(DaemonMessage::StateChanged { .. }))) => {
                // state transition may happen concurrently, continue waiting for conflict
            }
            _ => {}
        }
    }

    assert!(
        received_conflict,
        "expected to receive DaemonMessage::ConflictRecorded"
    );

    ipc_handle.shutdown();
    handle.shutdown();
}

#[tokio::test]
async fn test_in_memory_connection_handling() {
    let rig = TestRig::new();
    let handle = rig.engine.start();

    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        rig.store_dir.clone(),
        rig.tree_dir.clone(),
        rig.folder_id,
        rig.identity.clone(),
        tx,
    ));

    let (server_conn, mut client_conn) = create_in_memory_pair();

    let st = Arc::clone(&daemon_state);
    tokio::spawn(async move {
        handle_client_connection(server_conn, st).await;
    });

    // 1. Initial snapshot
    let initial = client_conn.recv_message().await.unwrap().unwrap();
    match initial {
        DaemonMessage::Snapshot(snap) => {
            assert_eq!(snap.folder, rig.tree_dir.display().to_string());
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    // 2. Ping -> Pong
    client_conn
        .send_command(&ClientCommand::Ping)
        .await
        .unwrap();
    let pong = client_conn.recv_message().await.unwrap().unwrap();
    assert_eq!(pong, DaemonMessage::Pong);

    handle.shutdown();
}
