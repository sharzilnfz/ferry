//! Keyboard event and hotkey action handler tests.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ferry_ipc::protocol::{ClientCommand, PinView};
use ferry_tui::state::SyncState;
use ferry_tui::TuiApp;

fn make_key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[test]
fn test_key_quit_q() {
    let mut app = TuiApp::default();
    assert!(!app.should_quit());

    let cmd = app.handle_key(make_key_event(KeyCode::Char('q'), KeyModifiers::NONE));
    assert!(cmd.is_none());
    assert!(app.should_quit());
}

#[test]
fn test_key_quit_q_uppercase() {
    let mut app = TuiApp::default();
    let cmd = app.handle_key(make_key_event(KeyCode::Char('Q'), KeyModifiers::NONE));
    assert!(cmd.is_none());
    assert!(app.should_quit());
}

#[test]
fn test_key_quit_esc() {
    let mut app = TuiApp::default();
    let cmd = app.handle_key(make_key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert!(cmd.is_none());
    assert!(app.should_quit());
}

#[test]
fn test_key_quit_ctrl_c() {
    let mut app = TuiApp::default();
    let cmd = app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(cmd.is_none());
    assert!(app.should_quit());

    let mut app2 = TuiApp::default();
    let cmd2 = app2.handle_key(make_key_event(KeyCode::Char('C'), KeyModifiers::CONTROL));
    assert!(cmd2.is_none());
    assert!(app2.should_quit());
}

#[test]
fn test_key_pin_toggle() {
    let mut app = TuiApp::default();
    app.state.engine_state = SyncState::Synced;
    app.state.pin = PinView::none();

    // Toggle on (StartPin) with lowercase 'p'
    let cmd = app.handle_key(make_key_event(KeyCode::Char('p'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::StartPin { paths: Vec::new() }));

    // Simulate pinned state
    app.state.engine_state = SyncState::Pinned;
    app.state.pin = PinView::active(vec!["file.txt".to_string()]);

    // Toggle off (ReleasePin) with uppercase 'P'
    let cmd = app.handle_key(make_key_event(KeyCode::Char('P'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::ReleasePin));
}

#[test]
fn test_key_rescan() {
    let mut app = TuiApp::default();
    let cmd = app.handle_key(make_key_event(KeyCode::Char('r'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::TriggerScan));

    let cmd_upper = app.handle_key(make_key_event(KeyCode::Char('R'), KeyModifiers::NONE));
    assert_eq!(cmd_upper, Some(ClientCommand::TriggerScan));
}

#[test]
fn test_key_conflicts_modal_toggle_and_dismiss_esc() {
    let mut app = TuiApp::default();
    assert!(!app.state.show_conflicts_modal);

    // Open modal with 'c'
    let cmd = app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::ListConflicts));
    assert!(app.state.show_conflicts_modal);

    // Press Esc to dismiss
    let cmd = app.handle_key(make_key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(cmd, None);
    assert!(!app.state.show_conflicts_modal);
    assert!(!app.should_quit()); // Esc inside modal should close modal, not quit app
}

#[test]
fn test_key_conflicts_modal_dismiss_q() {
    let mut app = TuiApp::default();
    app.handle_key(make_key_event(KeyCode::Char('C'), KeyModifiers::NONE));
    assert!(app.state.show_conflicts_modal);

    // Press 'q' inside modal to dismiss without quitting app
    let cmd = app.handle_key(make_key_event(KeyCode::Char('q'), KeyModifiers::NONE));
    assert_eq!(cmd, None);
    assert!(!app.state.show_conflicts_modal);
    assert!(!app.should_quit());

    // Reopen and test 'Q'
    app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(app.state.show_conflicts_modal);
    let cmd = app.handle_key(make_key_event(KeyCode::Char('Q'), KeyModifiers::NONE));
    assert_eq!(cmd, None);
    assert!(!app.state.show_conflicts_modal);
    assert!(!app.should_quit());
}

#[test]
fn test_key_conflicts_modal_dismiss_c() {
    let mut app = TuiApp::default();
    app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(app.state.show_conflicts_modal);

    // Press 'c' inside modal to dismiss without quitting app
    let cmd = app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert_eq!(cmd, None);
    assert!(!app.state.show_conflicts_modal);
    assert!(!app.should_quit());

    // Reopen and test 'C'
    app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(app.state.show_conflicts_modal);
    let cmd = app.handle_key(make_key_event(KeyCode::Char('C'), KeyModifiers::NONE));
    assert_eq!(cmd, None);
    assert!(!app.state.show_conflicts_modal);
    assert!(!app.should_quit());
}

#[test]
fn test_key_ctrl_c_inside_modal_quits() {
    let mut app = TuiApp::default();
    app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(app.state.show_conflicts_modal);

    // Ctrl+C while modal is active must still quit immediately
    let cmd = app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(cmd, None);
    assert!(app.should_quit());
}

#[test]
fn test_key_release_events_ignored() {
    let mut app = TuiApp::default();
    let release_event = KeyEvent {
        code: KeyCode::Char('q'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    };
    let cmd = app.handle_key(release_event);
    assert_eq!(cmd, None);
    assert!(!app.should_quit());
}

#[test]
fn test_unhandled_keys_ignored() {
    let mut app = TuiApp::default();
    let unhandled = [
        KeyCode::Char('x'),
        KeyCode::Char('1'),
        KeyCode::Enter,
        KeyCode::Down,
        KeyCode::Tab,
    ];
    for code in unhandled {
        let cmd = app.handle_key(make_key_event(code, KeyModifiers::NONE));
        assert_eq!(cmd, None);
        assert!(!app.should_quit());
    }
}

#[test]
fn test_apply_ack_list_conflicts_populates_entries() {
    use ferry_ipc::protocol::{ConflictEntry, DaemonMessage, DeviceStamp};

    let mut app = TuiApp::default();
    let sample_entries = vec![ConflictEntry {
        ts: "2026-08-26T12:00:00Z".to_string(),
        folder_id: "fid_123".to_string(),
        path: "docs/spec.md".to_string(),
        kind: "content".to_string(),
        winner: DeviceStamp {
            device: "device_a".to_string(),
            mtime_sec: Some(100),
            mtime_nsec: Some(0),
        },
        loser: DeviceStamp {
            device: "device_b".to_string(),
            mtime_sec: Some(90),
            mtime_nsec: Some(0),
        },
        quarantined_as: Some("docs/spec.sync-conflict.md".to_string()),
    }];
    let payload = serde_json::to_string(&sample_entries).unwrap();

    app.handle_message(DaemonMessage::Ack {
        command: "list_conflicts".to_string(),
        message: Some(payload),
    });

    assert_eq!(app.state.conflict_entries.len(), 1);
    assert_eq!(app.state.conflict_entries[0].path, "docs/spec.md");
    assert_eq!(app.state.conflicts, 1);
    assert_eq!(app.state.engine_state, SyncState::Conflict);
}
