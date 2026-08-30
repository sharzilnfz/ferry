use std::collections::HashMap;
use std::path::PathBuf;

use ferry_ipc::backend::{FakeBackend, InventoryDomain};
use ferry_ipc::protocol::{ClientCommand, DaemonMessage};
use ferry_ipc::{validate_path, DirectoryEntry};
use unicode_normalization::UnicodeNormalization;

#[tokio::test]
async fn fake_backend_lists_fixture_with_correct_flags() {
    let backend = FakeBackend::new();

    // Create fixture with distinct flags
    let dir = PathBuf::from("/tmp/fixture_root");
    let mut fixture = HashMap::new();
    fixture.insert(
        dir.clone(),
        vec![
            DirectoryEntry {
                name: "z_file.txt".to_string(),
                path: dir.join("z_file.txt"),
                is_dir: false,
                is_symlink: false,
                is_git_repo: false,
                is_already_synced: false,
                is_initialized: false,
            },
            DirectoryEntry {
                name: "a_dir".to_string(),
                path: dir.join("a_dir"),
                is_dir: true,
                is_symlink: false,
                is_git_repo: true,
                is_already_synced: true,
                is_initialized: false,
            },
            DirectoryEntry {
                name: "m_link".to_string(),
                path: dir.join("m_link"),
                is_dir: false,
                is_symlink: true,
                is_git_repo: false,
                is_already_synced: false,
                is_initialized: false,
            },
            DirectoryEntry {
                name: "b_dir".to_string(),
                path: dir.join("b_dir"),
                is_dir: true,
                is_symlink: false,
                is_git_repo: false,
                is_already_synced: false,
                is_initialized: false,
            },
        ],
    );
    backend.set_fs_fixture(fixture).await;

    let resp = backend
        .list_directory(Some(dir.clone()))
        .await
        .expect("listing");
    assert_eq!(resp.absolute_path, dir);
    // Stable sort: is_dir desc, then name asc => a_dir, b_dir, m_link, z_file.txt
    assert_eq!(resp.entries.len(), 4);
    assert_eq!(resp.entries[0].name, "a_dir");
    assert!(resp.entries[0].is_dir);
    assert!(resp.entries[0].is_git_repo);
    assert!(resp.entries[0].is_already_synced);
    assert_eq!(resp.entries[1].name, "b_dir");
    assert!(resp.entries[1].is_dir);
    assert!(!resp.entries[1].is_git_repo);
    assert_eq!(resp.entries[2].name, "m_link");
    assert!(resp.entries[2].is_symlink);
    assert!(!resp.entries[2].is_dir);
    assert_eq!(resp.entries[3].name, "z_file.txt");
}

#[tokio::test]
async fn fake_backend_traversal_protection() {
    let backend = FakeBackend::new();
    let mut fixture = HashMap::new();
    fixture.insert(
        PathBuf::from("/tmp"),
        vec![DirectoryEntry {
            name: "a".to_string(),
            path: PathBuf::from("/tmp/a"),
            is_dir: true,
            is_symlink: false,
            is_git_repo: false,
            is_already_synced: false,
            is_initialized: false,
        }],
    );
    backend.set_fs_fixture(fixture).await;

    let cases: Vec<PathBuf> = vec![
        PathBuf::from("/tmp/../etc/passwd"),
        PathBuf::from("/tmp//etc"),
        PathBuf::from("../../etc"),
    ];
    for p in cases {
        let err = backend.list_directory(Some(p.clone())).await.unwrap_err();
        assert!(
            err.code == "path-traversal" || err.code == "bad-path",
            "path {p:?} got {} expected traversal/bad-path",
            err.code
        );
        if err.code == "path-traversal" {
            assert_eq!(err.hint, "path escapes allowed root");
        }
    }
    // Non-absolute without traversal => bad-path
    let err = backend
        .list_directory(Some(PathBuf::from("relative/path")))
        .await
        .unwrap_err();
    assert_eq!(err.code, "bad-path");
}

#[tokio::test]
async fn fake_backend_not_found() {
    let backend = FakeBackend::new();
    let mut fixture = HashMap::new();
    fixture.insert(PathBuf::from("/exists"), vec![]);
    backend.set_fs_fixture(fixture).await;

    let err = backend
        .list_directory(Some(PathBuf::from("/nope")))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not-found");
}

#[test]
fn validate_path_helper_direct() {
    // absolute without traversal ok
    let p = validate_path(Some(PathBuf::from("/tmp/foo"))).unwrap();
    assert_eq!(p, PathBuf::from("/tmp/foo"));

    // traversal -> path-traversal
    let e = validate_path(Some(PathBuf::from("/tmp/../etc/passwd"))).unwrap_err();
    assert_eq!(e.code, "path-traversal");
    assert_eq!(e.hint, "path escapes allowed root");

    // non-absolute -> bad-path
    let e = validate_path(Some(PathBuf::from("relative"))).unwrap_err();
    assert_eq!(e.code, "bad-path");

    // double slash -> bad-path
    let e = validate_path(Some(PathBuf::from("/tmp//foo"))).unwrap_err();
    assert_eq!(e.code, "bad-path");

    // None defaults to current_dir or FERRY_HOME — should be absolute
    let p = validate_path(None).unwrap();
    assert!(p.is_absolute());
}

#[test]
fn ipc_round_trip_serialization() {
    let cmd = ClientCommand::ListDirectory {
        path: Some(PathBuf::from("/tmp")),
    };
    let json = serde_json::to_string(&cmd).unwrap();
    let back: ClientCommand = serde_json::from_str(&json).unwrap();
    assert_eq!(cmd, back);

    let msg = DaemonMessage::DirectoryListing {
        entries: vec![DirectoryEntry {
            name: "foo".to_string(),
            path: PathBuf::from("/tmp/foo"),
            is_dir: true,
            is_symlink: false,
            is_git_repo: false,
            is_already_synced: false,
            is_initialized: false,
        }],
        absolute_path: PathBuf::from("/tmp"),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let back: DaemonMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(msg, back);
}

#[test]
fn nfc_normalization_applied() {
    // Decomposed é (e +  combining) should be normalized to single é
    let decomposed = "e\u{0301}"; // é decomposed
    let nfc: String = decomposed.nfc().collect();
    assert_eq!(nfc, "é");
    let p = validate_path(Some(PathBuf::from(format!("/tmp/{decomposed}")))).unwrap();
    // Should contain NFC form
    assert!(p.to_string_lossy().contains('é'));
}

#[tokio::test]
async fn test_auto_backend_connect_auto_offline_fallback() {
    use ferry_ipc::backend::{connect_auto, StatusDomain};
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("my_folder");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("test.txt"), b"hello").unwrap();

    let socket_path = PathBuf::from("/tmp/nonexistent_socket_test_04.sock");
    let auto = connect_auto(socket_path, root.clone());

    // When daemon is offline, get_status returns an offline snapshot
    let status = auto.get_status().await.expect("offline status");
    assert_eq!(status.folder, root.display().to_string());
    assert_eq!(status.state, "offline");

    // list_directory falls back to local folder inspect
    let listing = auto
        .list_directory(Some(root.clone()))
        .await
        .expect("local listing");
    assert_eq!(listing.absolute_path, root);
    assert!(listing.entries.iter().any(|e| e.name == "test.txt"));
}

#[tokio::test]
async fn test_auto_backend_with_custom_fallback() {
    use ferry_ipc::backend::{AutoBackend, FakeBackend, StatusDomain};
    use std::sync::Arc;

    let socket_path = PathBuf::from("/tmp/nonexistent_socket_custom.sock");
    let fake = Arc::new(FakeBackend::new());
    let auto = AutoBackend::new(socket_path).with_fallback_backend(fake.clone());

    let status = auto.get_status().await.expect("fake backend status");
    assert_eq!(status.folder, "/test/folder");
}
