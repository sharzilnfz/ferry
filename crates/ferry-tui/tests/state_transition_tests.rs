use ferry_ipc::protocol::{
    DaemonMessage, EngineSnapshot, PeerStatusView, PinView, ScanStatsView, TransferDirection,
};
use ferry_tui::state::{SyncState, TuiState};
use ferry_tui::TuiApp;

#[test]
fn test_initial_state_defaults() {
    let state = TuiState::default();
    assert_eq!(state.engine_state, SyncState::Offline);
    assert_eq!(state.folder, "-");
    assert_eq!(state.cached_progress_label, "Idle (no active transfer)");
    assert_eq!(state.cached_progress_percent, 0);
}

#[test]
fn test_snapshot_transition() {
    let mut app = TuiApp::default();
    let mut snapshot =
        EngineSnapshot::new("/home/user/ferry_sync", "f_12345", "dev_abcdef", "synced");
    snapshot.manifest_id = Some("manifest_987654321".to_string());
    snapshot.scanned = ScanStatsView::new(100, 15, 0, 10_000_000);
    snapshot.pending_changes = Some(0);
    snapshot.peers = vec![PeerStatusView::new("peer_01", "reachable")];

    app.handle_message(DaemonMessage::Snapshot(snapshot));

    assert_eq!(app.state.folder, "/home/user/ferry_sync");
    assert_eq!(app.state.folder_id, "f_12345");
    assert_eq!(app.state.device_id, "dev_abcdef");
    assert_eq!(app.state.manifest_id, "manifest_987654321");
    assert_eq!(app.state.engine_state, SyncState::Synced);
    assert_eq!(app.state.scanned.files, 100);
    assert_eq!(app.state.peers.len(), 1);
    assert!(app.state.is_connected);
    assert!(app.state.cached_metrics_line.contains("100 files, 15 dirs"));
}

#[test]
fn test_state_changed_transition() {
    let mut app = TuiApp::default();

    app.handle_message(DaemonMessage::StateChanged {
        state: "syncing".to_string(),
        manifest_id: "m_new_root".to_string(),
        agreed_id: None,
        pending_changes: Some(5),
        stats: Some(ScanStatsView::new(105, 15, 0, 10_500_000)),
    });

    assert_eq!(app.state.engine_state, SyncState::Syncing);
    assert_eq!(app.state.manifest_id, "m_new_root");
    assert_eq!(app.state.pending_changes, Some(5));
    assert_eq!(app.state.scanned.files, 105);
}

#[test]
fn test_transfer_progress_and_completion() {
    let mut app = TuiApp::default();

    app.handle_message(DaemonMessage::TransferProgress {
        bytes_transferred: 4_000_000,
        total_bytes: 10_000_000,
        current_path: "assets/textures.pack".to_string(),
        chunks_transferred: Some(40),
        total_chunks: Some(100),
        peer_device_id: Some("peer_01".to_string()),
        direction: Some(TransferDirection::Sending),
    });

    assert_eq!(app.state.engine_state, SyncState::Syncing);
    assert!(app.state.active_transfer.is_some());
    assert_eq!(app.state.cached_progress_percent, 40);
    assert!(app.state.cached_progress_label.contains("Sending 40%"));
    assert!(app
        .state
        .cached_progress_label
        .contains("assets/textures.pack"));
    assert!(app.state.cached_progress_label.contains("[40/100 chunks]"));

    app.handle_message(DaemonMessage::TransferProgress {
        bytes_transferred: 10_000_000,
        total_bytes: 10_000_000,
        current_path: "assets/textures.pack".to_string(),
        chunks_transferred: Some(100),
        total_chunks: Some(100),
        peer_device_id: Some("peer_01".to_string()),
        direction: Some(TransferDirection::Sending),
    });

    assert!(app.state.active_transfer.is_none());
    assert_eq!(app.state.cached_progress_percent, 0);
    assert_eq!(app.state.cached_progress_label, "Idle (no active transfer)");
}

#[test]
fn test_conflict_recorded_transition() {
    let mut app = TuiApp::default();
    app.state.engine_state = SyncState::Synced;

    app.handle_message(DaemonMessage::ConflictRecorded {
        path: "src/config.rs".to_string(),
        conflict_path: "src/config.sync-conflict.rs".to_string(),
        timestamp: 1787574896,
        quarantined_as: Some("src/config.sync-conflict.rs".to_string()),
    });

    assert_eq!(app.state.conflicts, 1);
    assert_eq!(app.state.engine_state, SyncState::Conflict);
}

#[test]
fn test_pinned_state_transition() {
    let mut app = TuiApp::default();
    let mut snapshot = EngineSnapshot::new("/path", "f1", "d1", "synced");
    snapshot.pin = PinView::active(vec!["file1.txt".to_string()]);
    app.handle_message(DaemonMessage::Snapshot(snapshot));

    assert_eq!(app.state.engine_state, SyncState::Pinned);
    assert_eq!(app.state.cached_pin_line, "active (1 paths)");
}
