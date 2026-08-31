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
        is_initialized: true,
    }
}

fn uninitialized_entry(name: &str, path: &str) -> DirectoryEntry {
    DirectoryEntry {
        is_initialized: false,
        ..entry(name, path, true, false, false)
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

    assert_eq!(picker.entries[0].name, "docs");
    assert_eq!(picker.entries[1].name, "projects");

    picker.move_down();
    assert_eq!(picker.cursor, 1);
    picker.move_down();
    assert_eq!(picker.cursor, 2);

    assert_eq!(picker.visible_len(), 4);

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

    app.handle_key_action(&be, char_key('a')).await;
    assert!(app.is_picker_open(), "picker should be open via A");
    let cur = app.picker.as_ref().unwrap().current_path.clone();
    assert_eq!(cur, PathBuf::from("/"));

    app.handle_key_action(&be, key(KeyCode::Down)).await;
    app.handle_key_action(&be, key(KeyCode::Down)).await;
    let cursor = app.picker.as_ref().unwrap().cursor;
    assert_eq!(cursor, 2);

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

    assert_eq!(p.visible_len(), 4);

    p.apply_filter("pro");
    assert_eq!(p.visible_len(), 1);
    assert_eq!(p.visible_entries()[0].name, "projects");

    p.apply_filter("PRO");
    assert_eq!(p.visible_len(), 1);

    p.apply_filter("doc");
    assert_eq!(p.visible_len(), 1);
    assert_eq!(p.visible_entries()[0].name, "docs");

    p.clear_filter();
    assert_eq!(p.visible_len(), 4);
    assert_eq!(p.filter, "");
    assert_eq!(p.cursor, 0);

    p.push_filter_char('p');
    p.push_filter_char('r');
    p.push_filter_char('o');
    assert_eq!(p.filter, "pro");
    assert_eq!(p.visible_len(), 1);

    p.pop_filter_char();
    assert_eq!(p.filter, "pr");

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

    app.handle_key_action(&be, char_key('p')).await;
    app.handle_key_action(&be, char_key('r')).await;
    app.handle_key_action(&be, char_key('o')).await;
    let p = app.picker.as_ref().unwrap();
    assert_eq!(p.filter, "pro");
    assert_eq!(p.visible_len(), 1);
    assert_eq!(p.visible_entries()[0].name, "projects");

    app.handle_key_action(&be, key(KeyCode::Esc)).await;
    assert!(
        app.is_picker_open(),
        "Esc with filter should clear, not close"
    );
    assert_eq!(app.picker.as_ref().unwrap().filter, "");

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

    assert_eq!(p.entries[0].name, "a_dir");
    p.cursor = 0;
    let r = p.try_select();
    assert!(matches!(r, PickerSelectResult::Selected(e) if e.is_dir && e.name == "a_dir"));

    p.cursor = 1;

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

    let synced_idx = p
        .visible_entries()
        .iter()
        .position(|e| e.name == "synced")
        .unwrap();
    p.cursor = synced_idx;
    let r = p.try_select();
    assert!(matches!(r, PickerSelectResult::AlreadySynced(e) if e.name == "synced"));
    assert_eq!(p.hint.as_deref(), Some("already synced"));

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

    assert!(app.is_picker_open());
    assert_eq!(
        app.picker.as_ref().unwrap().hint.as_deref(),
        Some("already synced")
    );

    let has_warn = app
        .state
        .activity_log
        .entries()
        .iter()
        .any(|e| e.message.contains("already synced"));
    assert!(has_warn, "log should contain already synced hint");
}

#[test]
fn selection_uninitialized_dir_classifies_as_not_initialized() {
    let mut p = PickerState::new();
    p.set_entries(
        vec![
            uninitialized_entry("bare", "/bare"),
            entry("ready", "/ready", true, false, false),
        ],
        PathBuf::from("/"),
    );
    let bare_idx = p
        .visible_entries()
        .iter()
        .position(|e| e.name == "bare")
        .unwrap();
    p.cursor = bare_idx;
    let r = p.try_select();
    assert!(matches!(r, PickerSelectResult::NotInitialized(e) if e.name == "bare"));
    let hint = p.hint.as_deref().expect("hint for uninitialized pick");
    assert!(
        hint.contains("ferry init") && hint.contains("ferry pair"),
        "{hint}"
    );

    let ready_idx = p
        .visible_entries()
        .iter()
        .position(|e| e.name == "ready")
        .unwrap();
    p.cursor = ready_idx;
    let r2 = p.try_select();
    assert!(matches!(r2, PickerSelectResult::Selected(e) if e.name == "ready"));
    assert_eq!(p.hint, None);
}

#[tokio::test]
async fn app_space_on_uninitialized_dir_blocks_registration_with_banner() {
    let backend = Arc::new(FakeBackend::new());
    let mut fixture: HashMap<PathBuf, Vec<DirectoryEntry>> = HashMap::new();
    fixture.insert(
        PathBuf::from("/"),
        vec![
            uninitialized_entry("bare", "/bare"),
            entry("ready", "/ready", true, false, false),
        ],
    );
    backend.set_fs_fixture(fixture).await;
    let mut app = TuiApp::default();
    app.headless_override = Some(false);
    let be: Arc<dyn UiBackend> = backend.clone();
    app.handle_key_action(&be, char_key('a')).await;
    assert!(app.is_picker_open());

    app.handle_key_action(&be, key(KeyCode::Char(' '))).await;
    assert!(
        app.is_picker_open(),
        "picker stays open on blocked register"
    );
    let hint = app
        .picker
        .as_ref()
        .unwrap()
        .hint
        .as_deref()
        .expect("inline hint");
    assert!(
        hint.contains("ferry init") && hint.contains("ferry pair"),
        "{hint}"
    );
    let warned = app
        .state
        .activity_log
        .entries()
        .iter()
        .any(|e| e.message.contains("ferry init") || e.message.contains("ferry pair"));
    assert!(warned, "banner must name `ferry init` or `ferry pair`");
    let dispatched = app
        .state
        .activity_log
        .entries()
        .iter()
        .any(|e| e.message.contains("Register folder"));
    assert!(!dispatched, "registration must not reach the backend");

    app.handle_key_action(&be, key(KeyCode::Down)).await;
    app.handle_key_action(&be, key(KeyCode::Char(' '))).await;
    let reached_backend = app
        .state
        .activity_log
        .entries()
        .iter()
        .any(|e| e.message.contains("Register folder"));
    assert!(reached_backend, "initialized path dispatched to backend");
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

    assert!(ferry_tui::picker::is_headless_env("dumb", true));
    assert!(!ferry_tui::picker::is_headless_env("xterm-256color", true));
    assert!(ferry_tui::picker::is_headless_env("xterm-256color", false));

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

    p.open(Some(PathBuf::from("/tmp/foo/bar")));
    let parent = p.go_parent().unwrap();
    assert_eq!(parent, PathBuf::from("/tmp/foo"));

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
