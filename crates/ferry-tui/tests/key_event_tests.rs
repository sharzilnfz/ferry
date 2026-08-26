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
}

#[test]
fn test_key_pin_toggle() {
    let mut app = TuiApp::default();
    app.state.engine_state = SyncState::Synced;
    app.state.pin = PinView::none();

    // Toggle on (StartPin)
    let cmd = app.handle_key(make_key_event(KeyCode::Char('p'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::StartPin { paths: Vec::new() }));

    // Simulate pinned state
    app.state.engine_state = SyncState::Pinned;
    app.state.pin = PinView::active(vec!["file.txt".to_string()]);

    // Toggle off (ReleasePin)
    let cmd = app.handle_key(make_key_event(KeyCode::Char('p'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::ReleasePin));
}

#[test]
fn test_key_rescan() {
    let mut app = TuiApp::default();
    let cmd = app.handle_key(make_key_event(KeyCode::Char('r'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::TriggerScan));
}

#[test]
fn test_key_conflicts_modal_toggle() {
    let mut app = TuiApp::default();
    assert!(!app.state.show_conflicts_modal);

    // Open modal
    let cmd = app.handle_key(make_key_event(KeyCode::Char('c'), KeyModifiers::NONE));
    assert_eq!(cmd, Some(ClientCommand::ListConflicts));
    assert!(app.state.show_conflicts_modal);

    // Press Esc to dismiss
    let cmd = app.handle_key(make_key_event(KeyCode::Esc, KeyModifiers::NONE));
    assert_eq!(cmd, None);
    assert!(!app.state.show_conflicts_modal);
    assert!(!app.should_quit()); // Esc inside modal should close modal, not quit app
}
