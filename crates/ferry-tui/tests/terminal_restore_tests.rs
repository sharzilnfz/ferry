//! Terminal restoration, RAII cleanup, and panic hook safety tests.

use ferry_tui::terminal::{install_panic_hook, restore_terminal_writer};

#[test]
fn test_restore_terminal_writer_emits_expected_escapes() {
    let mut buffer = Vec::new();
    let res = restore_terminal_writer(&mut buffer);
    assert!(res.is_ok(), "restore_terminal_writer should succeed");

    // Check that buffer contains crossterm escape sequences for leave alternate screen,
    // disable mouse capture, and show cursor.
    let output = String::from_utf8_lossy(&buffer);
    eprintln!(
        "[DEBUG-b2] TERM={:?} buffer={:?}",
        std::env::var("TERM"),
        output
    );
    // Alternate screen leave sequence: \x1b[?1049l
    assert!(
        output.contains("\x1b[?1049l"),
        "Should contain LeaveAlternateScreen escape"
    );
    // Show cursor sequence: \x1b[?25h
    assert!(
        output.contains("\x1b[?25h"),
        "Should contain ShowCursor escape"
    );
}

#[test]
fn test_install_panic_hook_does_not_panic() {
    // Calling install_panic_hook should be safe and idempotent
    install_panic_hook();
    install_panic_hook();
}
