#![allow(clippy::field_reassign_with_default)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ferry_ipc::backend::{FakeBackend, UiBackend};
use ferry_ipc::DirectoryEntry;
use ferry_tui::app::TuiApp;
use ferry_tui::picker::{PickerSelectResult, PickerState};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn char_key(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}

fn entry(
    name: &str,
    path: &str,
    is_dir: bool,
    is_git_repo: bool,
    is_already_synced: bool,
) -> DirectoryEntry {
    DirectoryEntry {
        name: name.to_string(),
        path: PathBuf::from(path),
        is_dir,
        is_symlink: false,
        is_git_repo,
        is_already_synced,
    }
}

fn make_fixture() -> HashMap<PathBuf, Vec<DirectoryEntry>> {
    let mut m = HashMap::new();
    m.insert(
        PathBuf::from("/"),
        vec![
            entry("projects", "/projects", true, false, false),
            entry("docs", "/docs", true, true, false),
            entry("file.txt", "/file.txt", false, false, false),
            entry("synced_dir", "/synced_dir", true, false, true),
        ],
    );
    m.insert(
        PathBuf::from("/projects"),
        vec![
            entry("app", "/projects/app", true, false, false),
            entry("lib", "/projects/lib", true, false, false),
            entry("README.md", "/projects/README.md", false, false, false),
        ],
    );
    m.insert(
        PathBuf::from("/docs"),
        vec![entry("spec.md", "/docs/spec.md", false, false, false)],
    );
    m
}

#[tokio::test]
async fn picker_open_and_navigate_via_backend() {
    let backend = FakeBackend::new();
    backend.set_fs_fixture(make_fixture()).await;

    let mut picker = PickerState::new();
    picker
        .open_and_load(&backend, Some(PathBuf::from("/")))
        .await
        .unwrap();
    assert_eq!(picker.current_path, PathBuf::from("/"));
    assert!(!picker.loading);
    // Sorted: dirs first alphabetically (docs, projects, synced_dir) then files
    // sort_entries: is_dir desc then name asc => docs, projects, synced_dir, file.txt
    assert_eq!(picker.entries[0].name, "docs");
    assert_eq!(picker.entries[1].name, "projects");

    // Move down twice: cursor 0 -> 1 -> 2  (no filter)
    picker.move_down();
    assert_eq!(picker.cursor, 1);
    picker.move_down();
    assert_eq!(picker.cursor, 2);

    // Filter currently empty, visible_len 4
    assert_eq!(picker.visible_len(), 4);

    // Move down lands on synced_dir (index2) or file? depends ordering.
    // Enter on current selection if it's dir should return path
    // Let's explicitly set cursor to projects (index 1) and enter
    // Find projects entry position
    let proj_idx = picker
        .visible_entries()
        .iter()
        .position(|e| e.name == "projects")
        .unwrap();
    picker.cursor = proj_idx;
    let target = picker.enter().expect("projects is dir");
    assert_eq!(target, PathBuf::from("/projects"));

    picker.open_and_load(&backend, Some(target)).await.unwrap();
    assert_eq!(picker.current_path, PathBuf::from("/projects"));
    assert_eq!(picker.entries.len(), 3);
    assert!(picker.entries.iter().any(|e| e.name == "app"));
}

#[tokio::test]
async fn picker_enter_via_app_keyboard_simulation() {
    let backend = Arc::new(FakeBackend::new());
    backend.set_fs_fixture(make_fixture()).await;
    let mut app = TuiApp::default();
    app.headless_override = Some(false);
    let be: Arc<dyn UiBackend> = backend.clone();

    // Open via A (async handle_key_action)
    app.handle_key_action(&be, char_key('a')).await;
    assert!(app.is_picker_open(), "picker should be open via A");
    let cur = app.picker.as_ref().unwrap().current_path.clone();
    assert_eq!(cur, PathBuf::from("/"));

    // Move down twice via Down keys
    app.handle_key_action(&be, key(KeyCode::Down)).await;
    app.handle_key_action(&be, key(KeyCode::Down)).await;
    let cursor = app.picker.as_ref().unwrap().cursor;
    assert_eq!(cursor, 2);

    // Reset cursor to projects entry for deterministic enter
    {
        let p = app.picker.as_mut().unwrap();
        let idx = p
            .visible_entries()
            .iter()
            .position(|e| e.name == "projects")
            .unwrap();
        p.cursor = idx;
    }
    app.handle_key_action(&be, key(KeyCode::Enter)).await;
    assert_eq!(
        app.picker.as_ref().unwrap().current_path,
        PathBuf::from("/projects")
    );
    assert!(app
        .picker
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .any(|e| e.name == "app"));
}

#[tokio::test]
async fn filter_narrows_case_insensitive_and_esc_clears() {
    let backend = FakeBackend::new();
    backend.set_fs_fixture(make_fixture()).await;
    let mut p = PickerState::new();
    p.open_and_load(&backend, Some(PathBuf::from("/")))
        .await
        .unwrap();

    // Without filter, 4 entries
    assert_eq!(p.visible_len(), 4);
    // Apply filter "pro" -> should match "projects" only (case-insensitive)
    p.apply_filter("pro");
    assert_eq!(p.visible_len(), 1);
    assert_eq!(p.visible_entries()[0].name, "projects");

    // Case-insensitive: "PRO" same
    p.apply_filter("PRO");
    assert_eq!(p.visible_len(), 1);

    // Filter "DOC" matches docs
    p.apply_filter("doc");
    assert_eq!(p.visible_len(), 1);
    assert_eq!(p.visible_entries()[0].name, "docs");

    // Esc clears filter via clear_filter (simulating handle_key_action logic)
    p.clear_filter();
    assert_eq!(p.visible_len(), 4);
    assert_eq!(p.filter, "");
    assert_eq!(p.cursor, 0);

    // Typing via push_filter_char cumulative
    p.push_filter_char('p');
    p.push_filter_char('r');
    p.push_filter_char('o');
    assert_eq!(p.filter, "pro");
    assert_eq!(p.visible_len(), 1);

    // Backspace pops
    p.pop_filter_char();
    assert_eq!(p.filter, "pr");
    // "pr" matches "projects"
    assert_eq!(p.visible_len(), 1);

    p.pop_filter_char();
    p.pop_filter_char();
    assert_eq!(p.filter, "");
    assert_eq!(p.visible_len(), 4);
}

#[tokio::test]
async fn filter_via_app_typing_and_esc() {
    let backend = Arc::new(FakeBackend::new());
    backend.set_fs_fixture(make_fixture()).await;
    let mut app = TuiApp::default();
    app.headless_override = Some(false);
    let be: Arc<dyn UiBackend> = backend.clone();
    app.handle_key_action(&be, char_key('a')).await;
    assert!(app.is_picker_open());

    // Type "pro"
    app.handle_key_action(&be, char_key('p')).await;
    app.handle_key_action(&be, char_key('r')).await;
    app.handle_key_action(&be, char_key('o')).await;
    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.filter, "pro");
    assert_eq!(p.visible_len(), 1);
    assert_eq!(p.visible_entries()[0].name, "projects");

    // Esc should clear filter, not close modal
    app.handle_key_action(&be, key(KeyCode::Esc)).await;
    assert!(
        app.is_picker_open(),
        "Esc with filter should clear, not close"
    );
    assert_eq!(app.picker.as_ref().unwrap().filter, "");

    // Second Esc closes
    app.handle_key_action(&be, key(KeyCode::Esc)).await;
    assert!(!app.is_picker_open());
}

#[test]
fn selection_space_on_dir_vs_file() {
    let mut p = PickerState::new();
    p.set_entries(
        vec![
            entry("a_dir", "/a_dir", true, false, false),
            entry("file.txt", "/file.txt", false, false, false),
        ],
        PathBuf::from("/"),
    );
    // Sorted: dir first
    assert_eq!(p.entries[0].name, "a_dir");
    p.cursor = 0;
    let r = p.try_select();
    assert!(matches!(r, PickerSelectResult::Selected(e) if e.is_dir && e.name == "a_dir"));

    // Cursor on file does nothing
    p.cursor = 1;
    // Need to map visible entries; with no filter, cursor 1 is file.txt
    let r2 = p.try_select();
    assert_eq!(r2, PickerSelectResult::Nothing);
}

#[test]
fn selection_already_synced_shows_hint_without_register() {
    let mut p = PickerState::new();
    p.set_entries(
        vec![
            entry("synced", "/synced", true, false, true),
            entry("normal", "/normal", true, false, false),
        ],
        PathBuf::from("/"),
    );
    // Find synced index (order: normal, synced? Actually alphabetical: normal < synced, so normal first)
    // Let's locate synced
    let synced_idx = p
        .visible_entries()
        .iter()
        .position(|e| e.name == "synced")
        .unwrap();
    p.cursor = synced_idx;
    let r = p.try_select();
    assert!(matches!(r, PickerSelectResult::AlreadySynced(e) if e.name == "synced"));
    assert_eq!(p.hint.as_deref(), Some("already synced"));

    // Hint should persist, selection on normal should clear hint and return Selected
    let normal_idx = p
        .visible_entries()
        .iter()
        .position(|e| e.name == "normal")
        .unwrap();
    p.cursor = normal_idx;
    let r2 = p.try_select();
    assert!(matches!(r2, PickerSelectResult::Selected(e) if e.name == "normal"));
    assert_eq!(p.hint, None);
}

#[tokio::test]
async fn app_space_on_already_synced_shows_hint_no_register() {
    let backend = Arc::new(FakeBackend::new());
    backend.set_fs_fixture(make_fixture()).await;
    let mut app = TuiApp::default();
    app.headless_override = Some(false);
    let be: Arc<dyn UiBackend> = backend.clone();
    app.handle_key_action(&be, char_key('a')).await;
    assert!(app.is_picker_open());
    // Find synced_dir cursor
    {
        let p = app.picker.as_ref().unwrap();
        let idx = p
            .visible_entries()
            .iter()
            .position(|e| e.name == "synced_dir")
            .unwrap();
        app.picker.as_mut().unwrap().cursor = idx;
    }
    app.handle_key_action(&be, key(KeyCode::Char(' '))).await;
    // Should stay open and hint set
    assert!(app.is_picker_open());
    assert_eq!(
        app.picker.as_ref().unwrap().hint.as_deref(),
        Some("already synced")
    );
    // Activity log should contain already synced warning, not register success
    let has_warn = app
        .state
        .activity_log
        .entries()
        .iter()
        .any(|e| e.message.contains("already synced"));
    assert!(has_warn, "log should contain already synced hint");
}

#[tokio::test]
async fn headless_fallback_returns_no_tty() {
    let backend = Arc::new(FakeBackend::new());
    backend.set_fs_fixture(make_fixture()).await;
    let mut app = TuiApp::default();
    app.headless_override = Some(true);
    let be: Arc<dyn UiBackend> = backend.clone();

    let res = app.open_picker(&be, None).await;
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert_eq!(err.code, "no-tty");
    assert_eq!(err.hint, "pass explicit path");

    // Also via key handler: A should not open picker and should log error
    let mut app2 = TuiApp::default();
    app2.headless_override = Some(true);
    app2.handle_key_action(&be, char_key('a')).await;
    assert!(!app2.is_picker_open());
    let has_err = app2
        .state
        .activity_log
        .entries()
        .iter()
        .any(|e| e.message.contains("no-tty") || e.message.contains("no tty"));
    assert!(has_err);

    // Also verify TERM=dumb path via is_headless_env helper directly
    assert!(ferry_tui::picker::is_headless_env("dumb", true));
    assert!(!ferry_tui::picker::is_headless_env("xterm-256color", true));
    assert!(ferry_tui::picker::is_headless_env("xterm-256color", false));

    // headless error helper
    let e = ferry_tui::picker::headless_error();
    assert_eq!(e.code, "no-tty");
}

#[test]
fn picker_state_machine_unit() {
    let mut p = PickerState::new();
    p.open(Some(PathBuf::from("/tmp")));
    assert_eq!(p.current_path, PathBuf::from("/tmp"));
    assert!(p.loading);
    assert_eq!(p.cursor, 0);
    assert!(p.filter.is_empty());

    p.set_entries(
        vec![entry("b", "/tmp/b", true, true, false)],
        PathBuf::from("/tmp"),
    );
    assert!(!p.loading);
    assert_eq!(p.entries.len(), 1);
    assert_eq!(p.breadcrumbs(), "/tmp");

    p.apply_filter("b");
    assert_eq!(p.visible_len(), 1);
    p.apply_filter("nope");
    assert_eq!(p.visible_len(), 0);
    assert!(p.selected().is_none());

    // go_parent
    p.open(Some(PathBuf::from("/tmp/foo/bar")));
    let parent = p.go_parent().unwrap();
    assert_eq!(parent, PathBuf::from("/tmp/foo"));

    // move up/down wrapping
    p.set_entries(
        vec![
            entry("a", "/tmp/a", true, false, false),
            entry("b", "/tmp/b", true, false, false),
            entry("c", "/tmp/c", true, false, false),
        ],
        PathBuf::from("/tmp"),
    );
    p.cursor = 0;
    p.move_up();
    assert_eq!(p.cursor, 2);
    p.move_down();
    assert_eq!(p.cursor, 0);
}
