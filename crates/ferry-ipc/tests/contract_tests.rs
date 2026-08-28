use std::path::PathBuf;

use ferry_ipc::{
    backend::{FakeBackend, UiBackend},
    fs::{DirectoryEntry, ListDirectoryRequest, ListDirectoryResponse},
    pairing::{CreatePairingRequest, CreatePairingResponse, JoinPairingRequest, PairingCode},
    registry::{FolderRecord, FolderRegistry},
    ClientCommand, DaemonMessage, OpError,
};
use rand::SeedableRng;

#[test]
fn directory_entry_json_round_trip() {
    let entry = DirectoryEntry {
        name: "my-project".to_string(),
        path: PathBuf::from("/home/user/my-project"),
        is_dir: true,
        is_symlink: false,
        is_git_repo: true,
        is_already_synced: false,
    };
    let json = serde_json::to_string(&entry).expect("json ser");
    let back: DirectoryEntry = serde_json::from_str(&json).expect("json de");
    assert_eq!(entry, back);
}

#[test]
fn directory_entry_json_symlink_variant() {
    let entry = DirectoryEntry {
        name: "link".to_string(),
        path: PathBuf::from("/tmp/link"),
        is_dir: false,
        is_symlink: true,
        is_git_repo: false,
        is_already_synced: true,
    };
    let json = serde_json::to_string(&entry).unwrap();
    let back: DirectoryEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(entry, back);
}

#[test]
fn list_directory_request_round_trip() {
    let req_none = ListDirectoryRequest { path: None };
    let json = serde_json::to_string(&req_none).unwrap();
    let back: ListDirectoryRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(req_none, back);

    let req_some = ListDirectoryRequest {
        path: Some(PathBuf::from("/home/user")),
    };
    let json = serde_json::to_string(&req_some).unwrap();
    let back: ListDirectoryRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(req_some, back);
}

#[test]
fn list_directory_response_round_trip() {
    let resp = ListDirectoryResponse {
        entries: vec![DirectoryEntry {
            name: "a".to_string(),
            path: PathBuf::from("/tmp/a"),
            is_dir: true,
            is_symlink: false,
            is_git_repo: false,
            is_already_synced: false,
        }],
        absolute_path: PathBuf::from("/tmp"),
    };
    let json = serde_json::to_string(&resp).unwrap();
    let back: ListDirectoryResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(resp, back);
}

#[test]
fn folder_record_json_round_trip() {
    let rec = FolderRecord {
        folder_id: "0123456789abcdef0123456789abcdef".to_string(),
        path: PathBuf::from("/home/user/projects/foo"),
        added_at: "2026-08-28T12:00:00Z".to_string(),
    };
    let json = serde_json::to_string(&rec).unwrap();
    let back: FolderRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(rec, back);
}

#[test]
fn folder_registry_toml_round_trip() {
    let registry = FolderRegistry {
        folders: vec![
            FolderRecord {
                folder_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                path: PathBuf::from("/home/user/a"),
                added_at: "2026-08-28T12:00:00Z".to_string(),
            },
            FolderRecord {
                folder_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                path: PathBuf::from("/home/user/b"),
                added_at: "2026-08-28T13:00:00Z".to_string(),
            },
        ],
    };
    let toml_str = toml::to_string(&registry).expect("toml ser");
    let back: FolderRegistry = toml::from_str(&toml_str).expect("toml de");
    assert_eq!(registry, back);
}

#[test]
fn folder_registry_json_round_trip() {
    let registry = FolderRegistry {
        folders: vec![FolderRecord {
            folder_id: "cccccccccccccccccccccccccccccccc".to_string(),
            path: PathBuf::from("/tmp/c"),
            added_at: "2026-08-28T00:00:00Z".to_string(),
        }],
    };
    let json = serde_json::to_string(&registry).unwrap();
    let back: FolderRegistry = serde_json::from_str(&json).unwrap();
    assert_eq!(registry, back);
}

#[test]
fn create_pairing_request_round_trip() {
    let req = CreatePairingRequest {
        folder_id: "0123456789abcdef0123456789abcdef".to_string(),
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: CreatePairingRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
    let toml_str = toml::to_string(&req).unwrap();
    let back2: CreatePairingRequest = toml::from_str(&toml_str).unwrap();
    assert_eq!(req, back2);
}

#[test]
fn create_pairing_response_round_trip() {
    let resp = CreatePairingResponse {
        code: "ABC123".to_string(),
        expires_at: "2026-08-28T14:00:00Z".to_string(),
    };
    let json = serde_json::to_string(&resp).unwrap();
    let back: CreatePairingResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(resp, back);
}

#[test]
fn join_pairing_request_round_trip() {
    let req = JoinPairingRequest {
        code: "ABC123".to_string(),
        target_dir: PathBuf::from("/home/user/target"),
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: JoinPairingRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(req, back);
}

#[test]
fn pairing_code_generate_and_verify() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(42);
    let code = PairingCode::generate(&mut rng);
    assert_eq!(code.as_str().len(), 6);
    assert!(code.verify(code.as_str()));
    assert!(!code.verify("XXXXXX"));
    let mut rng2 = rand::rngs::StdRng::seed_from_u64(99);
    let code2 = PairingCode::generate(&mut rng2);
    assert_ne!(code.as_str(), code2.as_str());
    assert!(!code.verify(code2.as_str()));
}

#[test]
fn pairing_code_constant_time_mismatch_length() {
    let code = PairingCode::new("ABCDEF".to_string());
    assert!(!code.verify("ABC"));
    assert!(!code.verify("ABCDEFG"));
    assert!(code.verify("ABCDEF"));
}

#[test]
fn client_command_new_variants_round_trip() {
    let cmds = vec![
        ClientCommand::ListDirectory {
            path: Some(PathBuf::from("/tmp")),
        },
        ClientCommand::ListDirectory { path: None },
        ClientCommand::ListFolders,
        ClientCommand::RegisterFolder {
            path: PathBuf::from("/home/user/proj"),
        },
        ClientCommand::RemoveFolder {
            folder_id: "0123456789abcdef0123456789abcdef".to_string(),
        },
        ClientCommand::CreatePairingSession {
            req: CreatePairingRequest {
                folder_id: "0123456789abcdef0123456789abcdef".to_string(),
            },
        },
        ClientCommand::JoinPairingSession {
            req: JoinPairingRequest {
                code: "ABC123".to_string(),
                target_dir: PathBuf::from("/tmp/join"),
            },
        },
    ];
    for cmd in cmds {
        let json = serde_json::to_string(&cmd).unwrap();
        let back: ClientCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, back);
    }
}

#[test]
fn daemon_message_new_variants_round_trip() {
    let msgs = vec![
        DaemonMessage::DirectoryListing {
            entries: vec![DirectoryEntry {
                name: "foo".to_string(),
                path: PathBuf::from("/tmp/foo"),
                is_dir: true,
                is_symlink: false,
                is_git_repo: false,
                is_already_synced: false,
            }],
            absolute_path: PathBuf::from("/tmp"),
        },
        DaemonMessage::FolderList {
            folders: vec![FolderRecord {
                folder_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                path: PathBuf::from("/a"),
                added_at: "2026-08-28T12:00:00Z".to_string(),
            }],
        },
        DaemonMessage::FolderRegistered {
            folder: FolderRecord {
                folder_id: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                path: PathBuf::from("/b"),
                added_at: "2026-08-28T12:00:00Z".to_string(),
            },
        },
        DaemonMessage::FolderRemoved {
            folder_id: "cccccccccccccccccccccccccccccccc".to_string(),
        },
        DaemonMessage::PairingCreated {
            response: CreatePairingResponse {
                code: "XYZ789".to_string(),
                expires_at: "2026-08-28T15:00:00Z".to_string(),
            },
        },
        DaemonMessage::PairingJoined {
            result: ferry_ipc::PairResult {
                folder_id: "dddddddddddddddddddddddddddddddd".to_string(),
                device_id: "peer-1".to_string(),
                folder_path: PathBuf::from("/tmp/joined"),
                status: "paired".to_string(),
                message: None,
            },
        },
    ];
    for msg in msgs {
        let json = serde_json::to_string(&msg).unwrap();
        let back: DaemonMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}

#[test]
fn error_codes_table() {
    let cases = vec![
        ("bad-path", "path is invalid"),
        ("not-a-directory", "not a directory"),
        ("permission-denied", "permission denied"),
        ("path-traversal", "path traversal detected"),
        ("already-synced", "folder already synced"),
        ("not-found", "not found"),
        ("pairing-expired", "pairing code expired"),
        ("pairing-not-found", "pairing session not found"),
        ("secrets-found", "secrets detected"),
    ];
    for (code, msg) in cases {
        let err = OpError::new(code, msg, "hint");
        let json = serde_json::to_string(&err).unwrap();
        let back: OpError = serde_json::from_str(&json).unwrap();
        assert_eq!(back.code, code);
        assert_eq!(back.message, msg);
    }
}

#[test]
fn error_codes_specific_asserts() {
    let bad_path = OpError::new("bad-path", "bad path", "check path");
    assert_eq!(bad_path.code, "bad-path");

    let traversal = OpError::new("path-traversal", "traversal", "hint");
    assert_eq!(traversal.code, "path-traversal");

    let already = OpError::new("already-synced", "already synced", "hint");
    assert_eq!(already.code, "already-synced");

    let expired = OpError::new("pairing-expired", "expired", "hint");
    assert_eq!(expired.code, "pairing-expired");

    let err = DaemonMessage::Error {
        code: "bad-path".to_string(),
        message: "bad".to_string(),
    };
    let json = serde_json::to_string(&err).unwrap();
    assert!(json.contains("bad-path"));
}

#[tokio::test]
async fn fake_backend_stubs_return_not_implemented() {
    let backend = FakeBackend::new();

    let err = backend
        .list_directory(Some(PathBuf::from("/tmp")))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not-found");
    assert_eq!(err.message, "not-implemented");

    let err = backend.list_folders().await.unwrap_err();
    assert_eq!(err.code, "not-found");

    let err = backend
        .register_folder(PathBuf::from("/tmp/new"))
        .await
        .unwrap_err();
    assert_eq!(err.code, "not-found");

    let err = backend
        .remove_folder("0123456789abcdef0123456789abcdef".to_string())
        .await
        .unwrap_err();
    assert_eq!(err.code, "not-found");

    // Wave 3: pairing is now implemented via in-memory rendezvous (no files at $FERRY_HOME/pair-*)
    let resp = backend
        .create_pairing_session(CreatePairingRequest {
            folder_id: "0123456789abcdef0123456789abcdef".to_string(),
        })
        .await
        .expect("create pairing should succeed");
    assert_eq!(resp.code.len(), 6);
    let err = backend
        .join_pairing_session(JoinPairingRequest {
            code: "WRONG1".to_string(),
            target_dir: PathBuf::from("/tmp/join"),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "pairing-not-found");
    let ok = backend
        .join_pairing_session(JoinPairingRequest {
            code: resp.code.clone(),
            target_dir: PathBuf::from("/tmp/join-ok"),
        })
        .await
        .expect("join with correct code should succeed");
    assert_eq!(ok.folder_id, "0123456789abcdef0123456789abcdef");
    assert_eq!(ok.status, "paired");
    // Re-join same code must fail (one-time)
    let err = backend
        .join_pairing_session(JoinPairingRequest {
            code: resp.code,
            target_dir: PathBuf::from("/tmp/join-again"),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, "pairing-not-found");
}

#[tokio::test]
async fn fake_backend_trait_object_works() {
    let backend: Box<dyn UiBackend> = Box::new(FakeBackend::new());
    let err = backend.list_folders().await.unwrap_err();
    assert_eq!(err.code, "not-found");
    assert_eq!(err.message, "not-implemented");
}
