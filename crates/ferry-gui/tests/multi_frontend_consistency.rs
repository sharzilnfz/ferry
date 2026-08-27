//! End-to-end integration tests asserting multi-frontend consistency.
//!
//! Verifies that all 4 frontends (CLI status projection, TUI state, Web UI snapshot, and GUI state)
//! accurately and simultaneously reflect identical synchronized state transitions across:
//! - Initial synced state
//! - Rescan and pending file changes
//! - Transfer progress and syncing beacon states
//! - Pinning, held changes, and pin releases
//! - Conflict lifecycle (quarantine and resolution)

use std::sync::Arc;

use ferry_daemon::ui::backend::snapshot_to_status_doc;
use ferry_gui::beacon::BeaconState;
use ferry_gui::GuiApp;
use ferry_ipc::backend::{FakeBackend, UiBackend, UiEvent};
use ferry_ipc::protocol::{
    ConflictEntry, DeviceStamp, EngineSnapshot, PeerStatusView, ScanStatsView, TransferDirection,
};
use ferry_tui::state::SyncState;
use ferry_tui::TuiApp;

#[tokio::test]
async fn test_four_frontends_synced_state_consistency() {
    let fake = Arc::new(FakeBackend::new());
    let mut snap = EngineSnapshot::new("/workspace/project", "fold-1234", "dev-5678", "synced");
    snap.manifest_id = Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string());
    snap.scanned = ScanStatsView::new(250, 30, 4, 4_500_000);
    snap.peers.push(PeerStatusView::new("peer-node-alpha", "online"));
    fake.set_snapshot(snap.clone()).await;

    // 1. CLI Frontend projection
    let cli_snap = fake.get_status().await.unwrap();
    let cli_doc = snapshot_to_status_doc(&cli_snap);
    assert_eq!(cli_doc["command"], "status");
    assert_eq!(cli_doc["folder"], "/workspace/project");
    assert_eq!(cli_doc["folder_id"], "fold-1234");
    assert_eq!(cli_doc["device_id"], "dev-5678");
    assert_eq!(cli_doc["manifest_id"], "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(cli_doc["scanned"]["files"], 250);
    assert_eq!(cli_doc["scanned"]["dirs"], 30);
    assert_eq!(cli_doc["scanned"]["symlinks"], 4);
    assert_eq!(cli_doc["scanned"]["bytes_chunked"], 4_500_000);
    assert_eq!(cli_doc["pin"]["holding"], false);
    assert_eq!(cli_doc["held_changes"], 0);

    // 2. TUI Frontend state
    let mut tui_app = TuiApp::new_with_backend(fake.clone());
    tui_app.handle_ui_event(UiEvent::State(cli_snap.clone()));
    assert_eq!(tui_app.state.folder, "/workspace/project");
    assert_eq!(tui_app.state.folder_id, "fold-1234");
    assert_eq!(tui_app.state.engine_state, SyncState::Synced);
    assert_eq!(tui_app.state.scanned.files, 250);
    assert_eq!(tui_app.state.scanned.dirs, 30);
    assert_eq!(tui_app.state.scanned.symlinks, 4);
    assert_eq!(tui_app.state.scanned.bytes_chunked, 4_500_000);
    assert!(!tui_app.state.pin.holding);
    assert_eq!(tui_app.state.held_changes, 0);

    // 3. Web UI Dashboard projection
    let web_doc = snapshot_to_status_doc(&cli_snap);
    assert_eq!(web_doc, cli_doc);

    // 4. GUI Frontend state
    let mut gui_app = GuiApp::new_headless(fake.clone());
    gui_app.handle_event(UiEvent::State(cli_snap));
    assert_eq!(gui_app.beacon_state(), BeaconState::Synced);
    assert_eq!(gui_app.current_badge().0, "SYNCED");
    assert_eq!(gui_app.snapshot.as_ref().unwrap().folder, "/workspace/project");
    assert_eq!(gui_app.snapshot.as_ref().unwrap().folder_id, "fold-1234");
    assert_eq!(gui_app.snapshot.as_ref().unwrap().scanned.files, 250);
    assert_eq!(gui_app.snapshot.as_ref().unwrap().scanned.dirs, 30);
    assert_eq!(gui_app.snapshot.as_ref().unwrap().scanned.symlinks, 4);
    assert_eq!(gui_app.snapshot.as_ref().unwrap().scanned.bytes_chunked, 4_500_000);
    assert!(!gui_app.snapshot.as_ref().unwrap().pin.holding);
    assert_eq!(gui_app.snapshot.as_ref().unwrap().held_changes, 0);
}

#[tokio::test]
async fn test_four_frontends_rescan_and_file_change_transition() {
    let fake = Arc::new(FakeBackend::new());
    let mut snap = EngineSnapshot::new("/workspace/project", "fold-1234", "dev-5678", "synced");
    snap.scanned = ScanStatsView::new(100, 10, 0, 1_000_000);
    fake.set_snapshot(snap).await;

    let mut tui_app = TuiApp::new_with_backend(fake.clone());
    let mut gui_app = GuiApp::new_headless(fake.clone());

    // Trigger rescan
    fake.trigger_scan().await.unwrap();
    let updated_snap = fake.get_status().await.unwrap();
    assert_eq!(updated_snap.scanned.files, 101);

    // Broadcast state transition to all frontends
    let event = UiEvent::State(updated_snap.clone());

    // Update CLI
    let cli_doc = snapshot_to_status_doc(&updated_snap);
    assert_eq!(cli_doc["scanned"]["files"], 101);

    // Update TUI
    tui_app.handle_ui_event(event.clone());
    assert_eq!(tui_app.state.scanned.files, 101);

    // Update Web
    let web_doc = snapshot_to_status_doc(&updated_snap);
    assert_eq!(web_doc["scanned"]["files"], 101);

    // Update GUI
    gui_app.handle_event(event);
    assert_eq!(gui_app.snapshot.as_ref().unwrap().scanned.files, 101);
}

#[tokio::test]
async fn test_four_frontends_transfer_progress_and_syncing_transition() {
    let fake = Arc::new(FakeBackend::new());
    let snap = EngineSnapshot::new("/workspace/project", "fold-1234", "dev-5678", "synced");
    fake.set_snapshot(snap.clone()).await;

    let mut tui_app = TuiApp::new_with_backend(fake.clone());
    let mut gui_app = GuiApp::new_headless(fake.clone());
    tui_app.handle_ui_event(UiEvent::State(snap.clone()));
    gui_app.handle_event(UiEvent::State(snap));

    // Emit TransferProgress event
    let transfer_event = UiEvent::TransferProgress {
        bytes_transferred: 4_000_000,
        total_bytes: 10_000_000,
        current_path: "assets/textures/world.bin".to_string(),
        chunks_transferred: Some(40),
        total_chunks: Some(100),
        peer_device_id: Some("peer-delta-09".to_string()),
        direction: Some(TransferDirection::Sending),
    };

    // TUI state transition
    tui_app.handle_ui_event(transfer_event.clone());
    assert_eq!(tui_app.state.engine_state, SyncState::Syncing);
    assert_eq!(tui_app.state.cached_progress_percent, 40);

    // GUI state transition
    gui_app.handle_event(transfer_event);
    assert_eq!(gui_app.beacon_state(), BeaconState::Syncing);
    assert_eq!(gui_app.current_badge().0, "SYNCING");
    let transfer = gui_app.active_transfer.as_ref().unwrap();
    assert_eq!(transfer.bytes_transferred, 4_000_000);
    assert_eq!(transfer.total_bytes, 10_000_000);
    assert_eq!(transfer.current_path, "assets/textures/world.bin");
    assert_eq!(transfer.peer_device_id.as_deref(), Some("peer-delta-09"));
    assert_eq!(transfer.direction, Some(TransferDirection::Sending));
}

#[tokio::test]
async fn test_four_frontends_pin_hold_and_release_transition() {
    let fake = Arc::new(FakeBackend::new());
    let mut snap = EngineSnapshot::new("/workspace/project", "fold-1234", "dev-5678", "synced");
    snap.scanned = ScanStatsView::new(50, 5, 0, 500_000);
    fake.set_snapshot(snap.clone()).await;

    let mut tui_app = TuiApp::new_with_backend(fake.clone());
    let mut gui_app = GuiApp::new_headless(fake.clone());
    tui_app.handle_ui_event(UiEvent::State(snap.clone()));
    gui_app.handle_event(UiEvent::State(snap));

    // 1. Pin Start
    fake.start_pin(vec!["src/**".to_string()], None)
        .await
        .unwrap();

    let mut pinned_snap = fake.get_status().await.unwrap();
    pinned_snap.held_changes = 5;
    pinned_snap.state = "pinned".to_string();
    fake.set_snapshot(pinned_snap.clone()).await;

    let pin_event = UiEvent::State(pinned_snap.clone());

    // Check CLI
    let cli_doc = snapshot_to_status_doc(&pinned_snap);
    assert_eq!(cli_doc["pin"]["holding"], true);
    assert_eq!(cli_doc["held_changes"], 5);

    // Check TUI
    tui_app.handle_ui_event(pin_event.clone());
    assert_eq!(tui_app.state.engine_state, SyncState::Pinned);
    assert!(tui_app.state.pin.holding);
    assert_eq!(tui_app.state.held_changes, 5);

    // Check Web
    let web_doc = snapshot_to_status_doc(&pinned_snap);
    assert_eq!(web_doc["pin"]["holding"], true);
    assert_eq!(web_doc["held_changes"], 5);

    // Check GUI
    gui_app.handle_event(pin_event);
    assert_eq!(gui_app.beacon_state(), BeaconState::Holding);
    assert_eq!(gui_app.current_badge().0, "HOLDING");
    assert!(gui_app.snapshot.as_ref().unwrap().pin.holding);
    assert_eq!(gui_app.snapshot.as_ref().unwrap().held_changes, 5);

    // 2. Pin Release
    fake.release_pin().await.unwrap();
    let mut released_snap = fake.get_status().await.unwrap();
    released_snap.held_changes = 0;
    released_snap.state = "synced".to_string();
    fake.set_snapshot(released_snap.clone()).await;

    let release_event = UiEvent::State(released_snap.clone());

    // Check CLI
    let cli_released = snapshot_to_status_doc(&released_snap);
    assert_eq!(cli_released["pin"]["holding"], false);
    assert_eq!(cli_released["held_changes"], 0);

    // Check TUI
    tui_app.handle_ui_event(release_event.clone());
    assert_eq!(tui_app.state.engine_state, SyncState::Synced);
    assert!(!tui_app.state.pin.holding);
    assert_eq!(tui_app.state.held_changes, 0);

    // Check Web
    let web_released = snapshot_to_status_doc(&released_snap);
    assert_eq!(web_released["pin"]["holding"], false);
    assert_eq!(web_released["held_changes"], 0);

    // Check GUI
    gui_app.handle_event(release_event);
    assert_eq!(gui_app.beacon_state(), BeaconState::Synced);
    assert_eq!(gui_app.current_badge().0, "SYNCED");
    assert!(!gui_app.snapshot.as_ref().unwrap().pin.holding);
    assert_eq!(gui_app.snapshot.as_ref().unwrap().held_changes, 0);
}

#[tokio::test]
async fn test_four_frontends_conflict_lifecycle_transition() {
    let fake = Arc::new(FakeBackend::new());
    let snap = EngineSnapshot::new("/workspace/project", "fold-1234", "dev-5678", "synced");
    fake.set_snapshot(snap.clone()).await;

    let mut tui_app = TuiApp::new_with_backend(fake.clone());
    let mut gui_app = GuiApp::new_headless(fake.clone());
    tui_app.handle_ui_event(UiEvent::State(snap.clone()));
    gui_app.handle_event(UiEvent::State(snap));

    // 1. Conflict Recorded
    let conflict = ConflictEntry {
        ts: "2026-08-28T03:20:00Z".to_string(),
        folder_id: "fold-1234".to_string(),
        path: "src/engine.rs".to_string(),
        kind: "content".to_string(),
        winner: DeviceStamp {
            device: "device-local".to_string(),
            mtime_sec: Some(1787575000),
            mtime_nsec: None,
        },
        loser: DeviceStamp {
            device: "device-remote".to_string(),
            mtime_sec: Some(1787574900),
            mtime_nsec: None,
        },
        quarantined_as: Some("src/engine.rs.ferry-conflict.device-remote-1787574900".to_string()),
    };

    fake.add_conflict(conflict.clone()).await;

    let conflict_event = UiEvent::ConflictRecorded {
        path: conflict.path.clone(),
        conflict_path: conflict.quarantined_as.clone().unwrap(),
        timestamp: 1787575000,
        quarantined_as: conflict.quarantined_as.clone(),
    };

    // CLI backend query
    let cli_conflicts = fake.list_conflicts().await.unwrap();
    assert_eq!(cli_conflicts.len(), 1);
    assert_eq!(cli_conflicts[0].path, "src/engine.rs");

    // TUI state transition
    tui_app.handle_ui_event(conflict_event.clone());
    assert_eq!(tui_app.state.engine_state, SyncState::Conflict);
    assert_eq!(tui_app.state.conflicts, 1);

    // GUI state transition
    gui_app.handle_event(conflict_event);
    assert_eq!(gui_app.beacon_state(), BeaconState::Conflict);
    assert_eq!(gui_app.current_badge().0, "CONFLICT");
    assert_eq!(gui_app.conflicts.len(), 1);
    assert_eq!(gui_app.conflicts[0].path, "src/engine.rs");

    // 2. Conflict Resolved & Cleared
    let mut resolved_snap = fake.get_status().await.unwrap();
    resolved_snap.conflicts = 0;
    resolved_snap.state = "synced".to_string();
    fake.set_snapshot(resolved_snap.clone()).await;

    let resolved_event = UiEvent::State(resolved_snap.clone());

    // TUI
    tui_app.handle_ui_event(resolved_event.clone());
    assert_eq!(tui_app.state.engine_state, SyncState::Synced);

    // GUI
    gui_app.handle_event(resolved_event);
    gui_app.conflicts.clear();
    assert_eq!(gui_app.beacon_state(), BeaconState::Synced);
    assert_eq!(gui_app.current_badge().0, "SYNCED");
    assert_eq!(gui_app.conflicts.len(), 0);
}
