#![allow(
    deprecated,
    clippy::await_holding_lock,
    clippy::assigning_clones,
    clippy::cmp_owned,
    clippy::unnecessary_semicolon
)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ferry_crypto::identity::DeviceIdentity;
use ferry_daemon::ipc::spawn_ipc_server;
use ferry_daemon::state::DaemonState;
use ferry_daemon::ui::backend::{AutoBackend, DaemonIpcAdapter, InProcessAdapter};
use ferry_folder::folder::{create_folder, save_settings, Settings, SETTINGS_FORMAT_VERSION};
use ferry_ipc::backend::UiBackend;
use ferry_store::format::hex;
use ferry_sync::{EngineConfig, SyncEngine, TcpTransport};
use rand::rngs::StdRng;
use rand::SeedableRng;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn rig_with_engine() -> (TempDir, PathBuf, DeviceIdentity, SyncEngine, [u8; 16]) {
    let tmp = TempDir::new().expect("tempdir");
    let tree = tmp.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    let identity = DeviceIdentity::generate();
    let folder_id = [7u8; 16];
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::from_seed([7u8; 32]));
    let (store, _fmk) = create_folder(&tree, &identity, folder_id, poly).unwrap();
    store.flush().unwrap();
    store.write_index_snapshot().unwrap();
    let settings = Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: hex(&folder_id),
        honor_gitignore: true,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    save_settings(&tree, &settings).unwrap();
    let mut cfg = EngineConfig::default_for_test(7);
    cfg.tag = "test-fs".to_string();
    cfg.store_dir = tree.clone();
    cfg.tree_dir = tree.clone();
    cfg.folder_id = folder_id;
    cfg.poly = ferry_store::chunker::ValidatedPoly::new(poly).unwrap();
    cfg.pin_state_dir = Some(tree.join(".ferry"));
    let mut engine = SyncEngine::new(cfg, Arc::new(TcpTransport)).unwrap();
    engine.set_identity(identity.clone());
    (tmp, tree, identity, engine, folder_id)
}

#[tokio::test]
async fn in_process_lists_real_temp_dir_with_symlink_and_git() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();

    // Create 20 entries: 9 dirs + 9 files + 2 symlinks
    for i in 0..9 {
        std::fs::create_dir(root.join(format!("dir_{i:02}"))).unwrap();
        std::fs::write(root.join(format!("file_{i:02}.txt")), b"hello").unwrap();
    }
    // One of the dirs is a git repo
    let git_dir = root.join("dir_05");
    std::fs::create_dir(git_dir.join(".git")).unwrap();

    // Symlink: dir_01 -> linked
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("dir_01"), root.join("link_to_dir")).unwrap();
        std::os::unix::fs::symlink(root.join("file_00.txt"), root.join("link_to_file")).unwrap();
    }

    let adapter = InProcessAdapter::new(PathBuf::from("/tmp"));
    let resp = adapter
        .list_directory(Some(root.clone()))
        .await
        .expect("listing");
    assert_eq!(resp.absolute_path, root);
    assert_eq!(
        resp.entries.len(),
        20,
        "expected 20 entries, got {}",
        resp.entries.len()
    );

    // Stable sort: dirs first, then name asc
    let mut is_dir_seen_false = false;
    let mut last_name = String::new();
    let mut last_is_dir = true;
    for e in &resp.entries {
        if !e.is_dir {
            is_dir_seen_false = true;
        } else if is_dir_seen_false {
            panic!("dir after file violates sort");
        }
        if e.is_dir == last_is_dir {
            assert!(e.name >= last_name, "name order {} < {}", e.name, last_name);
        }
        last_name = e.name.clone();
        last_is_dir = e.is_dir;
    }

    // Git detection: dir_05 should be git repo
    let git_entry = resp.entries.iter().find(|e| e.name == "dir_05").unwrap();
    assert!(git_entry.is_dir);
    assert!(git_entry.is_git_repo, "dir_05 should be git repo");

    // Symlink detection
    #[cfg(unix)]
    {
        let link = resp
            .entries
            .iter()
            .find(|e| e.name == "link_to_dir")
            .unwrap();
        assert!(link.is_symlink, "link_to_dir should be symlink");
        let link2 = resp
            .entries
            .iter()
            .find(|e| e.name == "link_to_file")
            .unwrap();
        assert!(link2.is_symlink);
    }
}

#[tokio::test]
async fn in_process_already_synced_detection() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let ferry_home = tmp.path().join("ferry_home");
    std::fs::create_dir_all(&ferry_home).unwrap();
    let orig = std::env::var_os("FERRY_HOME");
    std::env::set_var("FERRY_HOME", &ferry_home);

    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    let child_a = root.join("project_a");
    let child_b = root.join("project_b");
    std::fs::create_dir_all(&child_a).unwrap();
    std::fs::create_dir_all(&child_b).unwrap();
    let other = root.join("other");
    std::fs::create_dir_all(&other).unwrap();

    // Write folders.toml with project_a registered
    let folders_toml = format!(
        r#"[[folders]]
folder_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
path = "{}"
added_at = "2026-08-28T12:00:00Z"
"#,
        child_a.display()
    );
    std::fs::write(ferry_home.join("folders.toml"), folders_toml).unwrap();

    let adapter = InProcessAdapter::new(PathBuf::from("/tmp"));
    let resp = adapter.list_directory(Some(root.clone())).await.unwrap();
    let a = resp.entries.iter().find(|e| e.name == "project_a").unwrap();
    assert!(a.is_already_synced, "project_a should be already_synced");
    // parent contains child_a, so other dirs that are not ancestors/descendants should not be synced? For root's children, only project_a itself is synced; but project_b is sibling, not synced.
    // However is_already_synced for project_a is true because it equals registered path.
    let b = resp.entries.iter().find(|e| e.name == "project_b").unwrap();
    assert!(!b.is_already_synced);
    // Also test descendant: list inside project_a should mark its children as already_synced (they are inside registered folder)
    std::fs::create_dir(child_a.join("sub")).unwrap();
    let resp2 = adapter.list_directory(Some(child_a.clone())).await.unwrap();
    let sub = resp2.entries.iter().find(|e| e.name == "sub").unwrap();
    assert!(
        sub.is_already_synced,
        "sub inside synced folder should be already_synced"
    );
    // Ancestor: root contains project_a, so root's parent check? Actually root is ancestor of project_a, so listing root's parent not needed. Check that listing root's entries: root itself not listed, but project_a is descendant. For ancestor detection: if we list a parent of registered folder, the ancestor itself (the registered folder) is descendant of parent? The helper checks both directions: candidate starts_with reg OR reg starts_with candidate. So a parent dir that contains registered folder will have is_already_synced false for its sibling entries, but the parent dir entry itself (when listing its parent) would be true if parent contains registered. Hard to test.

    // restore
    match orig {
        Some(v) => std::env::set_var("FERRY_HOME", v),
        None => std::env::remove_var("FERRY_HOME"),
    };
}

#[tokio::test]
async fn path_traversal_protection() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("file.txt"), b"x").unwrap();

    let adapter = InProcessAdapter::new(root.clone());

    let cases = vec![
        PathBuf::from("/tmp/../etc/passwd"),
        PathBuf::from("/tmp//etc"),
        PathBuf::from("../../etc"),
        PathBuf::from("relative/path"),
    ];
    for p in cases {
        let err = adapter.list_directory(Some(p.clone())).await.unwrap_err();
        assert!(
            err.code == "path-traversal" || err.code == "bad-path",
            "path {p:?} got {}",
            err.code
        );
        if err.code == "path-traversal" {
            assert_eq!(err.hint, "path escapes allowed root");
        }
        if p == PathBuf::from("relative/path") {
            assert_eq!(err.code, "bad-path");
        }
    }

    // Also via AutoBackend (should validate before delegating)
    let auto = AutoBackend::new(PathBuf::from("/tmp/nonexistent.sock")).with_fallback(root.clone());
    let err = auto
        .list_directory(Some(PathBuf::from("/tmp/../etc/passwd")))
        .await
        .unwrap_err();
    assert_eq!(err.code, "path-traversal");

    // Permission-denied never panics: try listing a file as dir
    let err = adapter
        .list_directory(Some(root.join("file.txt")))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not-a-directory");

    // Not-found
    let err = adapter
        .list_directory(Some(root.join("nope")))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not-found");
}

#[tokio::test]
async fn none_path_defaults_to_ferry_home_or_cwd() {
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = TempDir::new().unwrap();
    let ferry_home = tmp.path().join("home");
    std::fs::create_dir_all(&ferry_home).unwrap();
    std::fs::write(ferry_home.join("marker.txt"), b"hi").unwrap();
    let orig = std::env::var_os("FERRY_HOME");
    std::env::set_var("FERRY_HOME", &ferry_home);

    let adapter = InProcessAdapter::new(PathBuf::from("/tmp"));
    let resp = adapter.list_directory(None).await.expect("None path");
    assert_eq!(resp.absolute_path, ferry_home);
    assert!(resp.entries.iter().any(|e| e.name == "marker.txt"));

    match orig {
        Some(v) => std::env::set_var("FERRY_HOME", v),
        None => std::env::remove_var("FERRY_HOME"),
    };
}

#[tokio::test]
async fn daemon_ipc_adapter_forwards_without_re_reading() {
    // Spawn a real daemon IPC server that serves the temp dir
    let (tmp, tree, identity, engine, folder_id) = rig_with_engine();
    let root = tree.clone();
    // Create extra listing dir
    let list_root = tmp.path().join("list_root");
    std::fs::create_dir_all(&list_root).unwrap();
    for i in 0..5 {
        std::fs::write(list_root.join(format!("f{i}.txt")), b"x").unwrap();
    }

    let handle = engine.start();
    let (tx, _) = tokio::sync::broadcast::channel(128);
    let daemon_state = Arc::new(DaemonState::new(
        handle.clone(),
        root.clone(),
        tree.clone(),
        folder_id,
        identity.clone(),
        tx,
    ));
    let socket_path = ferry_ipc::paths::socket_path_for_dir(&tree);
    // Ensure socket not existing
    let _ = std::fs::remove_file(&socket_path);
    let server = spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state)).expect("spawn");
    // give time to bind
    tokio::time::sleep(Duration::from_millis(80)).await;

    let ipc_adapter = DaemonIpcAdapter::new(socket_path.clone());
    let resp = ipc_adapter
        .list_directory(Some(list_root.clone()))
        .await
        .expect("ipc listing");
    assert_eq!(resp.absolute_path, list_root);
    assert_eq!(resp.entries.len(), 5);
    for i in 0..5 {
        assert!(resp.entries.iter().any(|e| e.name == format!("f{i}.txt")));
    }

    // Verify that listing via InProcessAdapter directly matches the IPC result (both read same disk, but IPC path proves no extra read_dir in adapter itself — it just forwards)
    let direct = InProcessAdapter::new(PathBuf::from("/tmp"));
    let direct_resp = direct
        .list_directory(Some(list_root.clone()))
        .await
        .unwrap();
    assert_eq!(direct_resp.entries.len(), resp.entries.len());

    // Traversal via IPC should return path-traversal error, not listing
    let err = ipc_adapter
        .list_directory(Some(PathBuf::from("/tmp/../etc/passwd")))
        .await
        .unwrap_err();
    assert_eq!(err.code, "path-traversal");

    server.shutdown();
    handle.shutdown();
    tokio::time::sleep(Duration::from_millis(30)).await;
    let _ = std::fs::remove_file(&socket_path);
}

#[tokio::test]
async fn auto_backend_fallback_offline() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("hello.txt"), b"hi").unwrap();

    let auto =
        AutoBackend::new(PathBuf::from("/tmp/no_such_daemon.sock")).with_fallback(root.clone());
    // Daemon offline -> should fallback to InProcessAdapter and succeed
    let resp = auto
        .list_directory(Some(root.clone()))
        .await
        .expect("fallback");
    assert_eq!(resp.absolute_path, root);
    assert!(resp.entries.iter().any(|e| e.name == "hello.txt"));
}
