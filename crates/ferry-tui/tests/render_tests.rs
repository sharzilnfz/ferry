

use ferry_ipc::protocol::{
    ConflictEntry, DeviceStamp, PeerStatusView, PinView, ScanStatsView, TransferDirection,
};
use ferry_tui::state::{SyncState, TransferProgressState, TuiState};
use ferry_tui::TuiApp;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn setup_test_state(state: SyncState) -> TuiState {
    let mut s = TuiState::new(
        "/Users/dev/my-project",
        "folder_01abc",
        "device_fedcba9876543210",
    );
    s.manifest_id = "manifest_0123456789abcdef".to_string();
    s.scanned = ScanStatsView::new(42, 8, 2, 1024 * 1024 * 5 + 500 * 1024); 
    s.is_connected = true;
    s.engine_state = state;
    s.raw_state_str = state.badge_text().to_lowercase();

    match state {
        SyncState::Synced => {
            s.pending_changes = Some(0);
            s.pin = PinView::none();
            s.conflicts = 0;
            s.peers = vec![
                PeerStatusView {
                    device_id: "peer_node_1".to_string(),
                    last_agreed_manifest_id: Some("manifest_0123456789abcdef".to_string()),
                    agreed_at: Some("2026-08-26T12:00:00Z".to_string()),
                    connectivity: "reachable".to_string(),
                },
                PeerStatusView {
                    device_id: "peer_node_2".to_string(),
                    last_agreed_manifest_id: Some("manifest_prev12345678".to_string()),
                    agreed_at: Some("2026-08-26T11:45:00Z".to_string()),
                    connectivity: "unreachable".to_string(),
                },
            ];
        }
        SyncState::Syncing => {
            s.pending_changes = Some(3);
            s.active_transfer = Some(TransferProgressState {
                bytes_transferred: 2_500_000,
                total_bytes: 5_000_000,
                current_path: "src/video_stream.bin".to_string(),
                chunks_transferred: Some(25),
                total_chunks: Some(50),
                peer_device_id: Some("peer_node_1".to_string()),
                direction: Some(TransferDirection::Receiving),
            });
            s.peers = vec![PeerStatusView {
                device_id: "peer_node_1".to_string(),
                last_agreed_manifest_id: Some("manifest_0123456789abcdef".to_string()),
                agreed_at: Some("2026-08-26T12:00:00Z".to_string()),
                connectivity: "reachable".to_string(),
            }];
        }
        SyncState::Conflict => {
            s.conflicts = 2;
            s.conflict_entries = vec![ConflictEntry {
                ts: "2026-08-26T12:05:00Z".to_string(),
                folder_id: "folder_01abc".to_string(),
                path: "docs/architecture.md".to_string(),
                kind: "content".to_string(),
                winner: DeviceStamp {
                    device: "peer_node_1".to_string(),
                    mtime_sec: Some(1787574896),
                    mtime_nsec: Some(0),
                },
                loser: DeviceStamp {
                    device: "device_fedcba9876543210".to_string(),
                    mtime_sec: Some(1787574800),
                    mtime_nsec: Some(0),
                },
                quarantined_as: Some("docs/architecture.sync-conflict-20260826.md".to_string()),
            }];
        }
        SyncState::Pinned => {
            s.pin = PinView::active(vec!["src/main.rs".to_string(), "docs/spec.md".to_string()]);
            s.held_changes = 2;
        }
        _ => {}
    }

    s.update_cached_strings();
    s.activity_log
        .push_info("12:00:00", format!("Initial state: {}", state.badge_text()));
    s
}

fn buffer_to_string(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = buffer.cell((x, y)).expect("cell exists");
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn test_render_80x24_synced() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let state = setup_test_state(SyncState::Synced);
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(rendered.contains("Ferry Sync Engine"), "Missing title");
    assert!(rendered.contains("SYNCED"), "Missing SYNCED badge");
    assert!(rendered.contains("folder_01abc"), "Missing folder ID");
    assert!(
        rendered.contains("/Users/dev/my-project"),
        "Missing folder path"
    );
    assert!(
        rendered.contains("Storage & Sync State"),
        "Missing storage title"
    );
    assert!(
        rendered.contains("Connected Peers (2)"),
        "Missing peers count"
    );
    assert!(rendered.contains("Recent Activity"), "Missing activity log");
    assert!(rendered.contains("[P]"), "Missing [P] hotkey");
    assert!(rendered.contains("[R]"), "Missing [R] hotkey");
    assert!(rendered.contains("[C]"), "Missing [C] hotkey");
    assert!(rendered.contains("[Q]"), "Missing [Q] hotkey");
}

#[test]
fn test_render_80x24_syncing() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let state = setup_test_state(SyncState::Syncing);
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(rendered.contains("SYNCING"), "Missing SYNCING badge");
    assert!(
        rendered.contains("Transfer Progress"),
        "Missing transfer title"
    );
    assert!(
        rendered.contains("Receiving 50%"),
        "Missing transfer percentage"
    );
    assert!(rendered.contains("2.4 MB"), "Missing bytes transferred");
}

#[test]
fn test_render_80x24_conflict() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let state = setup_test_state(SyncState::Conflict);
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(rendered.contains("CONFLICT"), "Missing CONFLICT badge");
    assert!(rendered.contains("Conflicts:"), "Missing conflicts line");
    assert!(rendered.contains('2'), "Missing conflicts count");
}

#[test]
fn test_render_80x24_pinned() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let state = setup_test_state(SyncState::Pinned);
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(rendered.contains("PINNED"), "Missing PINNED badge");
    assert!(
        rendered.contains("active (2 paths)"),
        "Missing active pin details"
    );
}

#[test]
fn test_render_120x40_all_states() {
    let states = [
        SyncState::Synced,
        SyncState::Syncing,
        SyncState::Conflict,
        SyncState::Pinned,
    ];

    for st in states {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let state = setup_test_state(st);
        let app = TuiApp::new(state);

        terminal.draw(|f| app.render(f)).unwrap();

        let rendered = buffer_to_string(terminal.backend());
        assert!(
            rendered.contains(st.badge_text()),
            "120x40 grid missing badge: {}",
            st.badge_text()
        );
        assert!(
            rendered.contains("Ferry Sync Engine"),
            "120x40 missing header title"
        );
        assert!(
            rendered.contains("Recent Activity"),
            "120x40 missing activity log"
        );
        assert!(rendered.contains("[Q]"), "120x40 missing footer");
    }
}

#[test]
fn test_render_terminal_too_small() {
    let backend = TestBackend::new(30, 10);
    let mut terminal = Terminal::new(backend).unwrap();

    let state = setup_test_state(SyncState::Synced);
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(
        rendered.contains("Terminal too small"),
        "Should show too-small warning"
    );
}

#[test]
fn test_render_conflicts_modal() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut state = setup_test_state(SyncState::Conflict);
    state.show_conflicts_modal = true;
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(
        rendered.contains("Quarantined Conflicts"),
        "Missing modal title"
    );
    assert!(
        rendered.contains("docs/architecture.md"),
        "Missing conflict path"
    );
    assert!(
        rendered.contains("2026-08-26T12:05:00Z"),
        "Missing conflict timestamp"
    );
    assert!(
        rendered.contains("docs/architecture.sync-conflict-20260826.md"),
        "Missing quarantined file"
    );
    assert!(
        rendered.contains("Press [Esc], [Q], or [C] to close"),
        "Missing close hint"
    );
}

#[test]
fn test_render_conflicts_modal_empty() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut state = setup_test_state(SyncState::Synced);
    state.conflicts = 0;
    state.conflict_entries.clear();
    state.show_conflicts_modal = true;
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(
        rendered.contains("Quarantined Conflicts"),
        "Missing modal title"
    );
    assert!(
        rendered.contains("No quarantined conflict files detected"),
        "Missing empty message"
    );
    assert!(
        rendered.contains("Press [Esc], [Q], or [C] to return"),
        "Missing empty return hint"
    );
}

#[test]
fn test_render_empty_peers() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut state = setup_test_state(SyncState::Synced);
    state.peers.clear();
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(
        rendered.contains("Connected Peers (0)"),
        "Missing empty peers header"
    );
    assert!(
        rendered.contains("No peers connected"),
        "Missing empty peers placeholder"
    );
}
