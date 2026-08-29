//! Integration tests for `TuiApp` driven entirely by `Arc<dyn UiBackend>` and `UiEventStream`.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ferry_ipc::backend::{FakeBackend, StatusDomain, UiBackend, UiEvent};
use ferry_ipc::protocol::{
    ConflictEntry, DeviceStamp, EngineSnapshot, ScanStatsView, TransferDirection,
};
use ferry_tui::state::SyncState;
use ferry_tui::TuiApp;

fn make_key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[tokio::test]
async fn test_tuiapp_with_fake_backend_initialization_and_events() {
    let fake = Arc::new(FakeBackend::new());
    let mut snap = EngineSnapshot::new("/home/user/code", "fold123", "dev456", "synced");
    snap.scanned = ScanStatsView::new(50, 10, 1, 1024 * 1024);
    fake.set_snapshot(snap).await;

    let mut app = TuiApp::new_with_backend(fake.clone());
    assert_eq!(app.state.folder, "-");

    // Process snapshot event
    let snap_fresh = fake.get_status().await.unwrap();
    app.handle_ui_event(UiEvent::State(snap_fresh));
    assert_eq!(app.state.folder, "/home/user/code");
    assert_eq!(app.state.folder_id, "fold123");
    assert_eq!(app.state.engine_state, SyncState::Synced);

    // Process transfer progress event
    app.handle_ui_event(UiEvent::TransferProgress {
        bytes_transferred: 500_000,
        total_bytes: 1_000_000,
        current_path: "src/lib.rs".to_string(),
        chunks_transferred: Some(5),
        total_chunks: Some(10),
        peer_device_id: Some("peer1".to_string()),
        direction: Some(TransferDirection::Sending),
    });
    assert_eq!(app.state.engine_state, SyncState::Syncing);
    assert_eq!(app.state.cached_progress_percent, 50);

    // Process conflict event
    app.handle_ui_event(UiEvent::ConflictRecorded {
        path: "src/conflict.rs".to_string(),
        conflict_path: "src/conflict.sync-conflict.rs".to_string(),
        timestamp: 1787574896,
        quarantined_as: Some("src/conflict.sync-conflict.rs".to_string()),
    });
    assert_eq!(app.state.conflicts, 1);
    assert_eq!(app.state.engine_state, SyncState::Conflict);
}

#[tokio::test]
async fn test_tuiapp_keyboard_actions_against_fake_backend() {
    let fake = Arc::new(FakeBackend::new());
    let trait_backend: Arc<dyn UiBackend> = fake.clone();
    let mut app = TuiApp::new_with_backend(trait_backend.clone());

    // 1. Rescan action ('r')
    let snap_before = fake.get_status().await.unwrap();
    let files_before = snap_before.scanned.files;
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('r')))
        .await;
    let snap_after = fake.get_status().await.unwrap();
    assert_eq!(snap_after.scanned.files, files_before + 1);

    // 2. Pin start ('p')
    assert!(!app.state.pin.holding);
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('p')))
        .await;
    let snap_pinned = fake.get_status().await.unwrap();
    assert!(snap_pinned.pin.holding);

    // 3. Pin release ('p' while pinned)
    app.state.engine_state = SyncState::Pinned;
    app.state.pin = snap_pinned.pin;
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('p')))
        .await;
    let snap_released = fake.get_status().await.unwrap();
    assert!(!snap_released.pin.holding);

    // 4. Conflicts view ('c')
    fake.add_conflict(ConflictEntry {
        ts: "2026-08-28T00:00:00Z".to_string(),
        folder_id: "fold123".to_string(),
        path: "README.md".to_string(),
        kind: "content".to_string(),
        winner: DeviceStamp {
            device: "dev1".to_string(),
            mtime_sec: Some(100),
            mtime_nsec: Some(0),
        },
        loser: DeviceStamp {
            device: "dev2".to_string(),
            mtime_sec: Some(90),
            mtime_nsec: Some(0),
        },
        quarantined_as: Some("README.sync-conflict.md".to_string()),
    })
    .await;

    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('c')))
        .await;
    assert!(app.state.show_conflicts_modal);
    assert_eq!(app.state.conflict_entries.len(), 1);
    assert_eq!(app.state.conflict_entries[0].path, "README.md");

    // Close conflict modal with Esc
    app.handle_key_action(&trait_backend, make_key(KeyCode::Esc))
        .await;
    assert!(!app.state.show_conflicts_modal);
    assert!(!app.should_quit());

    // Quit with 'q'
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('q')))
        .await;
    assert!(app.should_quit());
}
