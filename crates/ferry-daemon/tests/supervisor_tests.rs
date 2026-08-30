#![allow(deprecated, clippy::ptr_as_ptr)]

use std::path::PathBuf;
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_daemon::supervisor::Supervisor;
use ferry_folder::folder::{save_settings, Settings, SETTINGS_FORMAT_VERSION};
use ferry_ipc::protocol::{ClientCommand, DaemonMessage};
use ferry_ipc::{create_in_memory_pair, default_socket_path};
use rand::rngs::StdRng;
use rand::SeedableRng;
use tempfile::TempDir;

fn tmp_home() -> TempDir {
    tempfile::tempdir().expect("home tempdir")
}

/// An initialized, device-shared folder: the only thing a supervisor engine
/// can open through ferry-folder.
fn init_folder(identity: &DeviceIdentity) -> TempDir {
    let dir = tempfile::tempdir().expect("folder tempdir");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&dir.path(), &mut h);
    let seed = std::hash::Hasher::finish(&h);
    let mut folder_id = [0u8; 16];
    folder_id[..8].copy_from_slice(&seed.to_le_bytes());
    folder_id[8..].copy_from_slice(&seed.to_be_bytes());
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(seed));
    let (store, _fmk) = ferry_folder::folder::create_folder(dir.path(), identity, folder_id, poly)
        .expect("init fixture folder");
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
    save_settings(
        dir.path(),
        &Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: ferry_store::format::hex(&folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )
    .unwrap();
    dir
}

fn new_supervisor(home: &TempDir) -> (Supervisor, DeviceIdentity) {
    let identity = DeviceIdentity::generate();
    (
        Supervisor::new(home.path().to_path_buf(), identity.clone()),
        identity,
    )
}

fn new_supervisor_with(home: &std::path::Path, identity: DeviceIdentity) -> Supervisor {
    Supervisor::new(home.to_path_buf(), identity)
}

#[tokio::test]
async fn supervisor_two_engines_distinct_status() {
    let home = tmp_home();
    let (mut sup, identity) = new_supervisor(&home);
    let dir_a = init_folder(&identity);
    let dir_b = init_folder(&identity);
    // register two folders via supervisor (needs runtime for spawn)
    let rec_a = sup
        .handle_register(dir_a.path().to_path_buf())
        .expect("register a");
    let rec_b = sup
        .handle_register(dir_b.path().to_path_buf())
        .expect("register b");
    assert_ne!(rec_a.folder_id, rec_b.folder_id);
    // wait for manifests
    assert!(
        sup.wait_for_manifests(Duration::from_secs(5)),
        "engines should produce manifests"
    );
    let snap_a = sup
        .get_status(Some(rec_a.folder_id.clone()))
        .expect("status a");
    let snap_b = sup
        .get_status(Some(rec_b.folder_id.clone()))
        .expect("status b");
    assert_eq!(snap_a.folder_id, rec_a.folder_id);
    assert_eq!(snap_b.folder_id, rec_b.folder_id);
    assert_ne!(snap_a.folder_id, snap_b.folder_id);
    // manifest_id should be Some after scan
    assert!(snap_a.manifest_id.is_some(), "manifest a");
    assert!(snap_b.manifest_id.is_some(), "manifest b");
    assert_ne!(
        snap_a.manifest_id, snap_b.manifest_id,
        "distinct manifests for distinct empty folders"
    );
    sup.shutdown();
}

#[tokio::test]
async fn register_adds_and_list_returns_three() {
    let home = tmp_home();
    let (mut sup, identity) = new_supervisor(&home);
    let dir_a = init_folder(&identity);
    let dir_b = init_folder(&identity);
    sup.handle_register(dir_a.path().to_path_buf()).unwrap();
    sup.handle_register(dir_b.path().to_path_buf()).unwrap();
    assert_eq!(sup.list_folders().len(), 2);
    // third registration
    let dir_c = init_folder(&identity);
    let rec_c = sup
        .handle_register(dir_c.path().to_path_buf())
        .expect("register c");
    let list = sup.list_folders();
    assert_eq!(list.len(), 3);
    assert!(list.iter().any(|r| r.folder_id == rec_c.folder_id));
    // also verify folders.toml on disk has 3
    let reg = ferry_folder::inventory::FolderInventory::new(home.path())
        .list()
        .unwrap();
    assert_eq!(reg.len(), 3);
    sup.shutdown();
}

#[tokio::test]
async fn remove_stops_and_list_returns_two() {
    let home = tmp_home();
    let (mut sup, identity) = new_supervisor(&home);
    let dir_a = init_folder(&identity);
    let dir_b = init_folder(&identity);
    let dir_c = init_folder(&identity);
    let rec_a = sup.handle_register(dir_a.path().to_path_buf()).unwrap();
    let rec_b = sup.handle_register(dir_b.path().to_path_buf()).unwrap();
    let rec_c = sup.handle_register(dir_c.path().to_path_buf()).unwrap();
    assert_eq!(sup.list_folders().len(), 3);
    // remove middle
    sup.handle_remove(&rec_b.folder_id).expect("remove b");
    let list = sup.list_folders();
    assert_eq!(list.len(), 2);
    assert!(!list.iter().any(|r| r.folder_id == rec_b.folder_id));
    assert!(list.iter().any(|r| r.folder_id == rec_a.folder_id));
    assert!(list.iter().any(|r| r.folder_id == rec_c.folder_id));
    // ensure engine for b is gone
    assert!(sup.get_engine_handle(&rec_b.folder_id).is_none());
    assert!(sup.get_engine_handle(&rec_a.folder_id).is_some());
    // folders.toml also 2
    let reg = ferry_folder::inventory::FolderInventory::new(home.path())
        .list()
        .unwrap();
    assert_eq!(reg.len(), 2);
    sup.shutdown();
}

#[test]
fn central_socket_default_path_respects_ferry_home() {
    // Serialize env access
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap();
    let orig = std::env::var_os("FERRY_HOME");
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("FERRY_HOME", tmp.path());
    let p = default_socket_path();
    assert_eq!(p, tmp.path().join("daemon.sock"));
    // socket_path_for_dir should be deprecated but still callable (allow)
    #[allow(deprecated)]
    let p2 = ferry_ipc::paths::socket_path_for_dir(&PathBuf::from("/any/folder"));
    // For deprecated wrapper, it still returns per-folder path, but we at least ensure default is global
    let _ = p2;
    // restore
    match orig {
        Some(v) => std::env::set_var("FERRY_HOME", v),
        None => std::env::remove_var("FERRY_HOME"),
    }
    // also verify default without FERRY_HOME falls back to HOME/.ferry or /tmp
    let p3 = default_socket_path();
    assert!(p3.ends_with("daemon.sock"));
    assert!(p3.to_string_lossy().contains(".ferry"));
}

#[tokio::test]
async fn ipc_dispatch_list_folders_over_loopback() {
    let home = tmp_home();
    let (mut sup, identity) = new_supervisor(&home);
    let dir_a = init_folder(&identity);
    let dir_b = init_folder(&identity);
    sup.handle_register(dir_a.path().to_path_buf()).unwrap();
    sup.handle_register(dir_b.path().to_path_buf()).unwrap();
    let expected_count = sup.list_folders().len();
    assert_eq!(expected_count, 2);

    let sup_arc = std::sync::Arc::new(tokio::sync::Mutex::new(sup));
    let (server_conn, mut client_conn) = create_in_memory_pair();
    let sup_clone = std::sync::Arc::clone(&sup_arc);
    tokio::spawn(async move {
        ferry_daemon::ipc::handle_supervisor_connection(server_conn, sup_clone).await;
    });
    // initial snapshot
    let init = client_conn.recv_message().await.unwrap().unwrap();
    assert!(matches!(init, DaemonMessage::Snapshot(_)));
    // list_folders
    client_conn
        .send_command(&ClientCommand::ListFolders)
        .await
        .unwrap();
    let resp = client_conn.recv_message().await.unwrap().unwrap();
    match resp {
        DaemonMessage::FolderList { folders } => {
            assert_eq!(folders.len(), expected_count);
        }
        other => panic!("expected FolderList, got {other:?}"),
    }
    // second call should still succeed without double disk read side effect
    client_conn
        .send_command(&ClientCommand::ListFolders)
        .await
        .unwrap();
    let resp2 = client_conn.recv_message().await.unwrap().unwrap();
    match resp2 {
        DaemonMessage::FolderList { folders } => assert_eq!(folders.len(), expected_count),
        other => panic!("expected FolderList second, got {other:?}"),
    }
    // shutdown
    sup_arc.lock().await.shutdown();
}

#[tokio::test]
async fn crashed_engine_restarts_and_other_unaffected() {
    let home = tmp_home();
    let (mut sup, identity) = new_supervisor(&home);
    let dir_a = init_folder(&identity);
    let dir_b = init_folder(&identity);
    let rec_a = sup.handle_register(dir_a.path().to_path_buf()).unwrap();
    let rec_b = sup.handle_register(dir_b.path().to_path_buf()).unwrap();
    assert!(sup.wait_for_manifests(Duration::from_secs(5)));
    let handle_a_before = sup.get_engine_handle(&rec_a.folder_id).unwrap();
    let handle_b_before = sup.get_engine_handle(&rec_b.folder_id).unwrap();
    let manifest_b_before = handle_b_before.current_manifest_id();

    handle_a_before.shutdown();
    sup.tick();

    let handle_a_after = sup.get_engine_handle(&rec_a.folder_id).unwrap();
    let handle_b_after = sup.get_engine_handle(&rec_b.folder_id).unwrap();
    assert_ne!(
        std::sync::Arc::as_ptr(&handle_a_before) as *const (),
        std::sync::Arc::as_ptr(&handle_a_after) as *const (),
        "crashed engine should be running on a fresh handle"
    );
    assert!(
        std::sync::Arc::ptr_eq(&handle_b_before, &handle_b_after),
        "other engine handle should be the same Arc"
    );
    assert!(handle_b_after.is_healthy(), "other engine still healthy");
    assert_eq!(
        handle_b_after.current_manifest_id(),
        manifest_b_before,
        "other engine untouched"
    );
    assert!(sup.wait_for_manifests(Duration::from_secs(5)));
    assert_eq!(sup.list_folders().len(), 2);
    sup.shutdown();
}

#[tokio::test]
async fn crash_restart_emits_backoff_event_and_escalates() {
    let home = tmp_home();
    let (mut sup, identity) = new_supervisor(&home);
    let dir_a = init_folder(&identity);
    let rec_a = sup.handle_register(dir_a.path().to_path_buf()).unwrap();
    assert!(sup.wait_for_manifests(Duration::from_secs(5)));
    let mut rx = sup.broadcast_tx().subscribe();

    sup.get_engine_handle(&rec_a.folder_id).unwrap().shutdown();
    sup.tick();
    let first = expect_engine_crashed(&mut rx).await;
    assert!(
        first.contains("100ms"),
        "first restart should record 100ms backoff, got: {first}"
    );
    assert!(sup.wait_for_manifests(Duration::from_secs(5)));

    sup.get_engine_handle(&rec_a.folder_id).unwrap().shutdown();
    sup.tick();
    let second = expect_engine_crashed(&mut rx).await;
    assert!(
        second.contains("200ms"),
        "second restart should double the backoff, got: {second}"
    );
    assert_eq!(sup.list_folders().len(), 1);
    sup.shutdown();
}

async fn expect_engine_crashed(
    rx: &mut tokio::sync::broadcast::Receiver<ferry_ipc::backend::UiEvent>,
) -> String {
    for _ in 0..20 {
        if let Ok(ferry_ipc::backend::UiEvent::Error { code, message }) = rx.try_recv() {
            if code == "engine-crashed" {
                return message;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no engine-crashed event received");
}

#[tokio::test]
async fn supervisor_spawn_engines_from_existing_registry() {
    // Test spawn_engines loads from folders.toml written externally
    let home = tmp_home();
    let identity = DeviceIdentity::generate();
    let dir_a = init_folder(&identity);
    let dir_b = init_folder(&identity);
    // manually seed the registry through the FolderInventory seam
    let inv = ferry_folder::inventory::FolderInventory::new(home.path());
    inv.register(dir_a.path()).unwrap();
    inv.register(dir_b.path()).unwrap();

    let mut sup = new_supervisor_with(home.path(), identity);
    sup.spawn_engines().expect("spawn");
    assert_eq!(sup.list_folders().len(), 2);
    assert_eq!(sup.engines_map().len(), 2);
    assert!(sup.wait_for_manifests(Duration::from_secs(5)));
    for (id, entry) in sup.engines_map() {
        let snap = sup.get_status(Some(id.clone())).unwrap();
        assert_eq!(snap.folder_id, *id);
        assert!(snap.manifest_id.is_some());
        let _ = entry;
    }
    sup.shutdown();
}

#[tokio::test]
async fn polynomial_stability_across_supervisor_instances() {
    let home1 = tmp_home();
    let identity1 = DeviceIdentity::generate();
    let home2 = tmp_home();
    let identity2 = DeviceIdentity::generate();

    let folder_id = [7u8; 16];
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(42));

    // Device 1
    let dir1 = tempfile::tempdir().expect("folder 1");
    let (store1, _) =
        ferry_folder::folder::create_folder(dir1.path(), &identity1, folder_id, poly).unwrap();
    store1.flush().unwrap();
    store1.write_index_snapshot().unwrap();
    save_settings(
        dir1.path(),
        &Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: ferry_store::format::hex(&folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )
    .unwrap();
    std::fs::write(dir1.path().join("file.txt"), b"deterministic payload").unwrap();

    // Device 2
    let dir2 = tempfile::tempdir().expect("folder 2");
    let (store2, _) =
        ferry_folder::folder::create_folder(dir2.path(), &identity2, folder_id, poly).unwrap();
    store2.flush().unwrap();
    store2.write_index_snapshot().unwrap();
    save_settings(
        dir2.path(),
        &Settings {
            format_version: SETTINGS_FORMAT_VERSION,
            folder_id: ferry_store::format::hex(&folder_id),
            honor_gitignore: false,
            presets: Vec::new(),
            overrides: Vec::new(),
        },
    )
    .unwrap();
    std::fs::write(dir2.path().join("file.txt"), b"deterministic payload").unwrap();

    // Spawn supervisor 1 on dir 1
    let mut sup1 = new_supervisor_with(home1.path(), identity1.clone());
    let rec1 = sup1
        .handle_register(dir1.path().to_path_buf())
        .expect("register in sup1");
    assert!(sup1.wait_for_manifests(Duration::from_secs(5)));
    let snap1 = sup1.get_status(Some(rec1.folder_id.clone())).unwrap();
    let manifest_id1 = snap1.manifest_id.expect("manifest1");
    let handle1 = sup1.get_engine_handle(&rec1.folder_id).unwrap();
    assert!(handle1.is_healthy());
    sup1.shutdown();

    // Spawn supervisor 2 on dir 2
    let mut sup2 = new_supervisor_with(home2.path(), identity2.clone());
    let rec2 = sup2
        .handle_register(dir2.path().to_path_buf())
        .expect("register in sup2");
    assert!(sup2.wait_for_manifests(Duration::from_secs(5)));
    let snap2 = sup2.get_status(Some(rec2.folder_id.clone())).unwrap();
    let manifest_id2 = snap2.manifest_id.expect("manifest2");
    let handle2 = sup2.get_engine_handle(&rec2.folder_id).unwrap();
    assert!(handle2.is_healthy());
    sup2.shutdown();

    // Verify chunks generated across instances match due to identical chunker polynomials
    let m1_id = ferry_store::format::unhex::<32>(&manifest_id1).expect("unhex m1");
    let m2_id = ferry_store::format::unhex::<32>(&manifest_id2).expect("unhex m2");

    let opened1 = ferry_folder::folder::open_folder(dir1.path(), &identity1).unwrap();
    let opened2 = ferry_folder::folder::open_folder(dir2.path(), &identity2).unwrap();

    assert_eq!(opened1.poly, poly);
    assert_eq!(opened2.poly, poly);

    let m1_bytes = opened1
        .store
        .get(ferry_store::format::BlobKind::Manifest, &m1_id)
        .unwrap();
    let m2_bytes = opened2
        .store
        .get(ferry_store::format::BlobKind::Manifest, &m2_id)
        .unwrap();
    let m1 = ferry_store::manifest::parse_manifest(&m1_bytes).unwrap();
    let m2 = ferry_store::manifest::parse_manifest(&m2_bytes).unwrap();

    let node1_bytes = opened1
        .store
        .get(ferry_store::format::BlobKind::TreeNode, &m1.root_tree_id)
        .unwrap();
    let node2_bytes = opened2
        .store
        .get(ferry_store::format::BlobKind::TreeNode, &m2.root_tree_id)
        .unwrap();
    let node1 = ferry_store::manifest::parse_tree_node(&node1_bytes).unwrap();
    let node2 = ferry_store::manifest::parse_tree_node(&node2_bytes).unwrap();

    assert_eq!(node1.entries[0].name, "file.txt");
    assert_eq!(node2.entries[0].name, "file.txt");
    assert_eq!(
        node1.entries[0].payload, node2.entries[0].payload,
        "chunks and blob hashes must be identical across devices when store polynomials match"
    );
}

#[tokio::test]
async fn uninitialized_folder_fails_supervisor_registration_loudly() {
    let home = tmp_home();
    let (mut sup, _identity) = new_supervisor(&home);
    let uninit_dir = tempfile::tempdir().expect("uninit tempdir");

    let err = sup
        .handle_register(uninit_dir.path().to_path_buf())
        .expect_err("uninitialized folder must fail registration");
    assert_eq!(err.code, "not-a-folder");
    sup.shutdown();
}

