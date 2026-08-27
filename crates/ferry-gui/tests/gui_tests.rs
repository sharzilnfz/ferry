//! Unit and headless render tests for `ferry-gui`.

use std::sync::Arc;

use ferry_gui::theme::{colors, Theme};
use ferry_gui::{format_bytes, GuiApp};
use ferry_ipc::backend::{FakeBackend, UiEvent};
use ferry_ipc::protocol::{EngineSnapshot, ScanStatsView, TransferDirection};

#[test]
fn test_theme_application_and_tokens() {
    let ctx = egui::Context::default();
    Theme::apply(&ctx);

    let visuals = ctx.style().visuals.clone();
    assert!(visuals.dark_mode);
    assert_eq!(visuals.panel_fill, colors::OBSIDIAN_BG);
    assert_eq!(visuals.window_fill, colors::CARD_BG);
}

#[test]
fn test_gui_format_bytes() {
    assert_eq!(format_bytes(500), "500 B");
    assert_eq!(format_bytes(1024), "1.0 KB");
    assert_eq!(format_bytes(1024 * 1024 * 5), "5.0 MB");
    assert_eq!(format_bytes(1024 * 1024 * 1024 * 2), "2.00 GB");
}

#[test]
fn test_gui_app_event_handling() {
    let fake = Arc::new(FakeBackend::new());
    let mut app = GuiApp::new_headless(fake);

    // Initial state
    assert_eq!(app.current_badge().0, "OFFLINE");

    // Receive snapshot
    let mut snap = EngineSnapshot::new("/test/folder", "folder123", "device456", "synced");
    snap.scanned = ScanStatsView::new(100, 20, 0, 10_000_000);
    app.handle_event(UiEvent::State(snap));

    assert_eq!(app.current_badge().0, "SYNCED");
    assert_eq!(app.snapshot.as_ref().unwrap().folder, "/test/folder");
    assert_eq!(app.snapshot.as_ref().unwrap().scanned.files, 100);

    // Receive transfer progress
    app.handle_event(UiEvent::TransferProgress {
        bytes_transferred: 5_000_000,
        total_bytes: 10_000_000,
        current_path: "data/file.bin".to_string(),
        chunks_transferred: Some(50),
        total_chunks: Some(100),
        peer_device_id: Some("peer1".to_string()),
        direction: Some(TransferDirection::Sending),
    });

    assert_eq!(app.current_badge().0, "SYNCING");
    assert!(app.active_transfer.is_some());
    assert_eq!(app.active_transfer.as_ref().unwrap().current_path, "data/file.bin");

    // Receive conflict
    app.handle_event(UiEvent::ConflictRecorded {
        path: "src/main.rs".to_string(),
        conflict_path: "src/main.sync-conflict.rs".to_string(),
        timestamp: 1787574896,
        quarantined_as: Some("src/main.sync-conflict.rs".to_string()),
    });

    assert_eq!(app.current_badge().0, "CONFLICT");
    assert_eq!(app.conflicts.len(), 1);
    assert_eq!(app.conflicts[0].path, "src/main.rs");
}
