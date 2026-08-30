use std::collections::HashMap;

use ferry_ipc::{
    ClientCommand, ConflictEntry, DaemonMessage, DeviceStamp, EngineSnapshot, PeerStatusView,
    PinView, ScanStatsView, TransferDirection,
};

#[test]
fn test_client_command_variants_serialization() {
    let commands = vec![
        (ClientCommand::GetStatus, r#"{"command":"get_status"}"#),
        (
            ClientCommand::StartPin {
                paths: vec!["src/**/*.rs".to_string(), "docs/*.md".to_string()],
                duration_hours: None,
            },
            r#"{"command":"start_pin","args":{"paths":["src/**/*.rs","docs/*.md"]}}"#,
        ),
        (
            ClientCommand::StartPin {
                paths: vec![],
                duration_hours: None,
            },
            r#"{"command":"start_pin","args":{"paths":[]}}"#,
        ),
        (ClientCommand::ReleasePin, r#"{"command":"release_pin"}"#),
        (ClientCommand::TriggerScan, r#"{"command":"trigger_scan"}"#),
        (
            ClientCommand::ListConflicts,
            r#"{"command":"list_conflicts"}"#,
        ),
        (ClientCommand::Ping, r#"{"command":"ping"}"#),
    ];

    for (cmd, expected_json) in commands {
        let serialized = serde_json::to_string(&cmd).expect("serialization failed");
        assert_eq!(serialized, expected_json);

        let deserialized: ClientCommand =
            serde_json::from_str(&serialized).expect("deserialization failed");
        assert_eq!(deserialized, cmd);
    }
}

#[test]
fn test_daemon_message_pong() {
    let msg = DaemonMessage::Pong;
    let serialized = serde_json::to_string(&msg).expect("serialization failed");
    assert_eq!(serialized, r#"{"type":"pong"}"#);

    let deserialized: DaemonMessage =
        serde_json::from_str(&serialized).expect("deserialization failed");
    assert_eq!(deserialized, msg);
}

#[test]
fn test_daemon_message_ack() {
    let msg1 = DaemonMessage::Ack {
        command: "start_pin".to_string(),
        message: Some("Pin acquired".to_string()),
    };
    let json1 = serde_json::to_string(&msg1).unwrap();
    let deserialized1: DaemonMessage = serde_json::from_str(&json1).unwrap();
    assert_eq!(deserialized1, msg1);

    let msg2 = DaemonMessage::Ack {
        command: "release_pin".to_string(),
        message: None,
    };
    let json2 = serde_json::to_string(&msg2).unwrap();
    let deserialized2: DaemonMessage = serde_json::from_str(&json2).unwrap();
    assert_eq!(deserialized2, msg2);
}

#[test]
fn test_daemon_message_error() {
    let msg = DaemonMessage::Error {
        code: "pin-failed".to_string(),
        message: "Active lock on tree".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let deserialized: DaemonMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, msg);
}

#[test]
fn test_daemon_message_state_changed() {
    let msg = DaemonMessage::StateChanged {
        state: "syncing".to_string(),
        manifest_id: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
        agreed_id: Some(
            "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210".to_string(),
        ),
        pending_changes: Some(3),
        stats: Some(ScanStatsView::new(10, 2, 1, 4096)),
    };

    let json = serde_json::to_string(&msg).unwrap();
    let deserialized: DaemonMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, msg);

    
    let minimal_json =
        r#"{"type":"state_changed","payload":{"state":"synced","manifest_id":"abc"}}"#;
    let deserialized_minimal: DaemonMessage = serde_json::from_str(minimal_json).unwrap();
    match deserialized_minimal {
        DaemonMessage::StateChanged {
            state,
            manifest_id,
            agreed_id,
            pending_changes,
            stats,
        } => {
            assert_eq!(state, "synced");
            assert_eq!(manifest_id, "abc");
            assert_eq!(agreed_id, None);
            assert_eq!(pending_changes, None);
            assert_eq!(stats, None);
        }
        _ => panic!("Expected StateChanged variant"),
    }
}

#[test]
fn test_daemon_message_transfer_progress() {
    let msg = DaemonMessage::TransferProgress {
        bytes_transferred: 512,
        total_bytes: 2048,
        current_path: "src/main.rs".to_string(),
        chunks_transferred: Some(2),
        total_chunks: Some(8),
        peer_device_id: Some("deadbeef1234".to_string()),
        direction: Some(TransferDirection::Receiving),
    };

    let json = serde_json::to_string(&msg).unwrap();
    let deserialized: DaemonMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, msg);

    
    let minimal_json = r#"{"type":"transfer_progress","payload":{"bytes_transferred":100,"total_bytes":500,"current_path":"data.bin"}}"#;
    let deserialized_minimal: DaemonMessage = serde_json::from_str(minimal_json).unwrap();
    match deserialized_minimal {
        DaemonMessage::TransferProgress {
            bytes_transferred,
            total_bytes,
            current_path,
            chunks_transferred,
            total_chunks,
            peer_device_id,
            direction,
        } => {
            assert_eq!(bytes_transferred, 100);
            assert_eq!(total_bytes, 500);
            assert_eq!(current_path, "data.bin");
            assert_eq!(chunks_transferred, None);
            assert_eq!(total_chunks, None);
            assert_eq!(peer_device_id, None);
            assert_eq!(direction, None);
        }
        _ => panic!("Expected TransferProgress variant"),
    }
}

#[test]
fn test_daemon_message_conflict_recorded() {
    let msg = DaemonMessage::ConflictRecorded {
        path: "notes.txt".to_string(),
        conflict_path: "notes.txt.ferry-conflict.node-b-1698765432".to_string(),
        timestamp: 1698765432,
        quarantined_as: Some("notes.txt.ferry-conflict.node-b-1698765432".to_string()),
    };

    let json = serde_json::to_string(&msg).unwrap();
    let deserialized: DaemonMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, msg);
}

#[test]
fn test_daemon_message_snapshot() {
    let mut held_by_peer = HashMap::new();
    held_by_peer.insert("peer1".to_string(), vec!["doc1.txt".to_string()]);

    let mut snapshot =
        EngineSnapshot::new("/home/user/project", "folder123", "device456", "synced");
    snapshot.manifest_id = Some("root_manifest_hash_64_hex".to_string());
    snapshot.scanned = ScanStatsView::new(42, 5, 0, 1024 * 1024);
    snapshot.pending_changes = Some(0);
    snapshot.pin = PinView::active(vec!["secret.env".to_string()]);
    snapshot.held_changes = 1;
    snapshot.held_by_peer = held_by_peer;
    snapshot.peers = vec![PeerStatusView {
        device_id: "peer1".to_string(),
        last_agreed_manifest_id: Some("root_manifest_hash_64_hex".to_string()),
        agreed_at: Some("2026-08-26T12:00:00Z".to_string()),
        connectivity: "reachable".to_string(),
    }];
    snapshot.conflicts = 0;

    let msg = DaemonMessage::Snapshot(snapshot.clone());
    let json = serde_json::to_string(&msg).unwrap();
    let deserialized: DaemonMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, msg);

    if let DaemonMessage::Snapshot(s) = deserialized {
        assert_eq!(s.folder, "/home/user/project");
        assert_eq!(s.pin.state, "active");
        assert!(s.pin.holding);
        assert_eq!(s.pin.paths, vec!["secret.env"]);
        assert_eq!(s.peers.len(), 1);
        assert_eq!(s.peers[0].connectivity, "reachable");
    } else {
        panic!("Expected Snapshot variant");
    }
}

#[test]
fn test_conflict_entry_and_device_stamp() {
    let entry = ConflictEntry {
        ts: "2026-08-26T12:30:00Z".to_string(),
        folder_id: "folder_1".to_string(),
        path: "README.md".to_string(),
        kind: "both_changed".to_string(),
        winner: DeviceStamp {
            device: "node-a".to_string(),
            mtime_sec: Some(1700000000),
            mtime_nsec: Some(500),
        },
        loser: DeviceStamp {
            device: "node-b".to_string(),
            mtime_sec: Some(1699999990),
            mtime_nsec: Some(200),
        },
        quarantined_as: Some("README.md.ferry-conflict.node-b-1700000000".to_string()),
    };

    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: ConflictEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, entry);
}
