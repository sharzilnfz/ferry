use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_daemon::ipc::spawn_ipc_server;
use ferry_daemon::registry::FolderRegistry;
use ferry_daemon::state::DaemonState;
use ferry_daemon::ui::backend::{AutoBackend, InProcessAdapter};
use ferry_ipc::backend::{FolderInfo, UiBackend};
use ferry_ipc::protocol::{ClientCommand, DaemonMessage, PinView};
use ferry_ipc::IpcClient;
use ferry_sync::TcpTransport;
use tempfile::TempDir;

struct MultiFolderTestEnv {
    _temp_dir: TempDir,
    home_dir: PathBuf,
    folder_a: PathBuf,
    folder_b: PathBuf,
    identity: DeviceIdentity,
}

impl MultiFolderTestEnv {
    fn new() -> Self {
        let temp_dir = TempDir::new().expect("tempdir");
        let home_dir = temp_dir.path().join("ferry_home");
        let folder_a = temp_dir.path().join("workspace_a");
        let folder_b = temp_dir.path().join("workspace_b");

        std::fs::create_dir_all(&home_dir).expect("create home");
        std::fs::create_dir_all(&folder_a).expect("create folder_a");
        std::fs::create_dir_all(&folder_b).expect("create folder_b");

        let identity = DeviceIdentity::generate();

        Self {
            _temp_dir: temp_dir,
            home_dir,
            folder_a,
            folder_b,
            identity,
        }
    }

    fn socket_path(&self) -> PathBuf {
        self.home_dir.join("daemon.sock")
    }
}

#[test]
fn test_folder_registry_persistence_and_lifecycle() {
    let env = MultiFolderTestEnv::new();
    let toml_path = env.home_dir.join("folders.toml");

    let mut reg = FolderRegistry::new();
    let id_a = "0102030405060708090a0b0c0d0e0f10".to_string();
    let id_b = "1112131415161718191a1b1c1d1e1f20".to_string();

    let entry_a = reg.register(id_a.clone(), env.folder_a.clone());
    assert_eq!(entry_a.id, id_a);
    assert!(entry_a.active);

    let entry_b = reg.register(id_b.clone(), env.folder_b.clone());
    assert_eq!(entry_b.id, id_b);
    assert!(!entry_b.active);

    reg.save_to_file(&toml_path).expect("save registry");
    assert!(toml_path.exists(), "folders.toml must exist");

    // Load from disk and verify entries
    let reloaded = FolderRegistry::load_from_file(&toml_path).expect("load registry");
    assert_eq!(reloaded.list().len(), 2);
    assert_eq!(reloaded.active_folder().unwrap().id, id_a);

    // Switch active folder
    let mut reg_mut = reloaded;
    let switched = reg_mut.switch(&id_b).expect("switch folder");
    assert_eq!(switched.id, id_b);
    assert!(switched.active);
    assert!(!reg_mut.get(&id_a).unwrap().active);
    reg_mut.save_to_file(&toml_path).expect("save switched");

    // Reload again to verify switch persistence
    let reloaded_2 = FolderRegistry::load_from_file(&toml_path).expect("load registry 2");
    assert_eq!(reloaded_2.active_folder().unwrap().id, id_b);

    // Unregister folder A
    let mut reg_mut_2 = reloaded_2;
    let removed = reg_mut_2.unregister(&id_a).expect("unregister folder A");
    assert_eq!(removed.id, id_a);
    assert_eq!(reg_mut_2.list().len(), 1);
    reg_mut_2.save_to_file(&toml_path).expect("save after unregister");

    let final_reg = FolderRegistry::load_from_file(&toml_path).expect("load final");
    assert_eq!(final_reg.list().len(), 1);
    assert_eq!(final_reg.list()[0].id, id_b);
}

#[tokio::test]
async fn test_central_daemon_multi_folder_ipc_supervision() {
    let env = MultiFolderTestEnv::new();

    let (broadcast_tx, _) = tokio::sync::broadcast::channel(256);
    let daemon_state = Arc::new(DaemonState::with_home_and_transport(
        env.home_dir.clone(),
        env.identity.clone(),
        Arc::new(TcpTransport),
        broadcast_tx,
    ));

    let socket_path = env.socket_path();
    let ipc_handle =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    let mut client = IpcClient::connect(&socket_path)
        .await
        .expect("client connect");

    // 1. Initial snapshot from empty daemon
    let initial_msg = client.recv_message().await.unwrap().unwrap();
    assert!(matches!(initial_msg, DaemonMessage::Snapshot(_)));

    // 2. Register Folder A
    client
        .send_command(&ClientCommand::RegisterFolder {
            path: env.folder_a.display().to_string(),
        })
        .await
        .unwrap();

    let folder_a_id: String;
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, message } if command == "register_folder" => {
                let info: FolderInfo = serde_json::from_str(&message.unwrap()).unwrap();
                assert_eq!(info.path, env.folder_a);
                assert!(info.active);
                folder_a_id = info.id;
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }
    assert!(!folder_a_id.is_empty());

    // 3. Register Folder B
    client
        .send_command(&ClientCommand::RegisterFolder {
            path: env.folder_b.display().to_string(),
        })
        .await
        .unwrap();

    let folder_b_id: String;
    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, message } if command == "register_folder" => {
                let info: FolderInfo = serde_json::from_str(&message.unwrap()).unwrap();
                assert_eq!(info.path, env.folder_b);
                assert!(info.active);
                folder_b_id = info.id;
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }
    assert!(!folder_b_id.is_empty());
    assert_ne!(folder_a_id, folder_b_id);

    // 4. List Folders -> Both must be returned
    client
        .send_command(&ClientCommand::ListFolders)
        .await
        .unwrap();

    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, message } if command == "list_folders" => {
                let folders: Vec<FolderInfo> = serde_json::from_str(&message.unwrap()).unwrap();
                assert_eq!(folders.len(), 2);
                let found_a = folders.iter().find(|f| f.id == folder_a_id).unwrap();
                let found_b = folders.iter().find(|f| f.id == folder_b_id).unwrap();
                assert_eq!(found_a.path, env.folder_a);
                assert_eq!(found_b.path, env.folder_b);
                assert!(found_b.active);
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }

    // 5. Start a Pin on Folder B (currently active)
    client
        .send_command(&ClientCommand::StartPin {
            paths: vec!["sub/file.txt".to_string()],
            duration_hours: None,
        })
        .await
        .unwrap();

    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, .. } if command == "start_pin" => break,
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }

    // 6. Switch to Folder A -> Verify snapshot is Folder A and has NO pin
    client
        .send_command(&ClientCommand::SwitchFolder {
            folder_id: folder_a_id.clone(),
        })
        .await
        .unwrap();

    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Snapshot(snap) => {
                assert_eq!(snap.folder_id, folder_a_id);
                assert_eq!(snap.folder, env.folder_a.display().to_string());
                assert_eq!(snap.pin, PinView::none(), "Folder A must not inherit Folder B's pin");
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }

    // 7. Verify folders.toml on disk is updated with active folder A
    let toml_path = env.home_dir.join("folders.toml");
    let on_disk_reg = FolderRegistry::load_from_file(&toml_path).expect("load on-disk registry");
    assert_eq!(on_disk_reg.list().len(), 2);
    assert_eq!(on_disk_reg.active_folder().unwrap().id, folder_a_id);

    // 8. Unregister Folder A -> Active folder switches back to Folder B
    client
        .send_command(&ClientCommand::UnregisterFolder {
            folder_id: folder_a_id.clone(),
        })
        .await
        .unwrap();

    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, message } if command == "unregister_folder" => {
                assert_eq!(message.as_deref(), Some(folder_a_id.as_str()));
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }

    // Verify list only has Folder B
    client
        .send_command(&ClientCommand::ListFolders)
        .await
        .unwrap();

    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, message } if command == "list_folders" => {
                let folders: Vec<FolderInfo> = serde_json::from_str(&message.unwrap()).unwrap();
                assert_eq!(folders.len(), 1);
                assert_eq!(folders[0].id, folder_b_id);
                assert!(folders[0].active);
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }

    // Test ListDirectory via IPC
    client
        .send_command(&ClientCommand::ListDirectory {
            path: Some(env.home_dir.parent().unwrap().display().to_string()),
        })
        .await
        .unwrap();

    loop {
        match client.recv_message().await.unwrap().unwrap() {
            DaemonMessage::Ack { command, message } if command == "list_directory" => {
                let listing: ferry_ipc::backend::DirectoryListing =
                    serde_json::from_str(&message.unwrap()).unwrap();
                assert!(listing.entries.iter().any(|e| e.name == "workspace_a"));
                assert!(listing.entries.iter().any(|e| e.name == "workspace_b"));
                break;
            }
            DaemonMessage::StateChanged { .. } => {}
            other => panic!("unexpected msg: {other:?}"),
        }
    }

    drop(client);
    ipc_handle.shutdown();
}

#[tokio::test]
async fn test_backend_adapters_multi_folder_operations() {
    let env = MultiFolderTestEnv::new();

    // 1. Test InProcessAdapter multi-folder registration and listing
    let in_proc = InProcessAdapter::new(&env.folder_a)
        .with_home(env.home_dir.clone())
        .with_identity(env.identity.clone());

    let reg_a = in_proc
        .register_folder(env.folder_a.clone())
        .await
        .expect("in_proc register a");
    assert_eq!(reg_a.path, env.folder_a);

    let reg_b = in_proc
        .register_folder(env.folder_b.clone())
        .await
        .expect("in_proc register b");
    assert_eq!(reg_b.path, env.folder_b);

    let folders = in_proc.list_folders().await.expect("in_proc list folders");
    assert_eq!(folders.len(), 2);

    let switched_snap = in_proc
        .switch_folder(reg_a.id.clone())
        .await
        .expect("in_proc switch to a");
    assert_eq!(switched_snap.folder_id, reg_a.id);

    // 2. Start Daemon IPC and test AutoBackend and DaemonIpcAdapter
    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::with_home_and_transport(
        env.home_dir.clone(),
        env.identity.clone(),
        Arc::new(TcpTransport),
        tx,
    ));

    let socket_path = env.socket_path();
    let ipc_handle =
        spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn ipc");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let auto = AutoBackend::new(socket_path.clone())
        .with_home(env.home_dir.clone())
        .with_fallback(env.folder_a.clone())
        .with_identity(env.identity.clone());

    let auto_folders = auto.list_folders().await.expect("auto list folders");
    assert_eq!(auto_folders.len(), 2);

    let folder_c = env.home_dir.parent().unwrap().join("workspace_c");
    std::fs::create_dir_all(&folder_c).unwrap();

    let reg_c = auto
        .register_folder(folder_c.clone())
        .await
        .expect("auto register c");
    assert_eq!(reg_c.path, folder_c);

    let auto_folders_updated = auto.list_folders().await.expect("auto list folders updated");
    assert_eq!(auto_folders_updated.len(), 3);

    auto.unregister_folder(reg_c.id.clone())
        .await
        .expect("auto unregister c");

    let auto_folders_final = auto.list_folders().await.expect("auto list folders final");
    assert_eq!(auto_folders_final.len(), 2);

    ipc_handle.shutdown();
}
