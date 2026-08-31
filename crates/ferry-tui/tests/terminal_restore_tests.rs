use ferry_tui::terminal::{install_panic_hook, restore_terminal_writer};

#[test]
fn test_restore_terminal_writer_emits_expected_escapes() {
    let mut buffer = Vec::new();
    let res = restore_terminal_writer(&mut buffer);
    assert!(res.is_ok(), "restore_terminal_writer should succeed");

    let output = String::from_utf8_lossy(&buffer);

    assert!(
        output.contains("\x1b[?1049l"),
        "Should contain LeaveAlternateScreen escape"
    );

    assert!(
        output.contains("\x1b[?25h"),
        "Should contain ShowCursor escape"
    );
}

#[test]
fn test_install_panic_hook_does_not_panic() {
    install_panic_hook();
    install_panic_hook();
}
