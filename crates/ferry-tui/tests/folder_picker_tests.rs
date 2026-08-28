//! Tests for the interactive filesystem explorer / folder picker modal.

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use ferry_ipc::backend::{DirectoryListing, FakeBackend, FsEntry, UiBackend};
use ferry_ipc::protocol::ClientCommand;
use ferry_tui::state::{FolderPickerItem, FolderPickerState, TuiState};
use ferry_tui::TuiApp;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn make_key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn sample_listing() -> DirectoryListing {
    DirectoryListing {
        current_path: PathBuf::from("/home/user/workspace"),
        parent_path: Some(PathBuf::from("/home/user")),
        entries: vec![
            FsEntry {
                name: "ferry-project".to_string(),
                path: PathBuf::from("/home/user/workspace/ferry-project"),
                is_dir: true,
                is_symlink: false,
                is_git_repo: true,
                is_already_synced: false,
            },
            FsEntry {
                name: "synced-docs".to_string(),
                path: PathBuf::from("/home/user/workspace/synced-docs"),
                is_dir: true,
                is_symlink: false,
                is_git_repo: false,
                is_already_synced: true,
            },
            FsEntry {
                name: "shared-symlink".to_string(),
                path: PathBuf::from("/home/user/workspace/shared-symlink"),
                is_dir: true,
                is_symlink: true,
                is_git_repo: false,
                is_already_synced: false,
            },
            FsEntry {
                name: "notes.txt".to_string(),
                path: PathBuf::from("/home/user/workspace/notes.txt"),
                is_dir: false,
                is_symlink: false,
                is_git_repo: false,
                is_already_synced: false,
            },
        ],
    }
}

#[test]
fn test_folder_picker_state_filtering_and_navigation() {
    let mut state = FolderPickerState::default();
    state.set_listing(sample_listing());

    // Initially: [..], ferry-project, synced-docs, shared-symlink, notes.txt (5 items)
    assert_eq!(state.filtered_items().len(), 5);
    assert_eq!(state.selected_index, 0);
    assert_eq!(
        state.selected_item(),
        Some(FolderPickerItem::Parent(PathBuf::from("/home/user")))
    );

    // Down navigation
    state.move_selection_down();
    assert_eq!(state.selected_index, 1);
    if let Some(FolderPickerItem::Entry(e)) = state.selected_item() {
        assert_eq!(e.name, "ferry-project");
    } else {
        panic!("Expected ferry-project entry");
    }

    // Up navigation
    state.move_selection_up();
    assert_eq!(state.selected_index, 0);

    // Bounding at top
    state.move_selection_up();
    assert_eq!(state.selected_index, 0);

    // Filter typing
    state.append_filter('s');
    state.append_filter('y');
    state.append_filter('n');
    // Filtered should match "synced-docs"
    let filtered = state.filtered_items();
    assert_eq!(filtered.len(), 1);
    if let FolderPickerItem::Entry(ref e) = filtered[0] {
        assert_eq!(e.name, "synced-docs");
    } else {
        panic!("Expected synced-docs");
    }

    // Backspace
    state.backspace_filter(); // 'syn' -> 'sy'
    let filtered2 = state.filtered_items();
    // 'synced-docs' and 'shared-symlink' match 'sy'
    assert_eq!(filtered2.len(), 2);

    // Clear filter
    state.clear_filter();
    assert_eq!(state.filtered_items().len(), 5);
}

#[tokio::test]
async fn test_tuiapp_open_folder_picker_with_a_and_o() {
    let fake = Arc::new(FakeBackend::new());
    let trait_backend: Arc<dyn UiBackend> = fake.clone();

    // Test 'a'
    let mut app = TuiApp::new_with_backend(trait_backend.clone());
    assert!(!app.state.show_folder_picker_modal);
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('a')))
        .await;
    assert!(app.state.show_folder_picker_modal);
    assert!(!app.state.folder_picker.raw_entries.is_empty());

    // Dismiss with Esc
    app.handle_key_action(&trait_backend, make_key(KeyCode::Esc))
        .await;
    assert!(!app.state.show_folder_picker_modal);
    assert!(!app.should_quit());

    // Test 'O'
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('O')))
        .await;
    assert!(app.state.show_folder_picker_modal);

    // Dismiss with 'q' when filter is empty
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char('q')))
        .await;
    assert!(!app.state.show_folder_picker_modal);
    assert!(!app.should_quit());
}

#[tokio::test]
async fn test_tuiapp_folder_picker_navigation_and_selection_space() {
    let fake = Arc::new(FakeBackend::new());
    let trait_backend: Arc<dyn UiBackend> = fake.clone();
    let mut app = TuiApp::new_with_backend(trait_backend.clone());

    app.state.folder_picker.set_listing(sample_listing());
    app.state.show_folder_picker_modal = true;

    // Navigate down to "ferry-project" (index 1)
    app.handle_key_action(&trait_backend, make_key(KeyCode::Down))
        .await;
    assert_eq!(app.state.folder_picker.selected_index, 1);

    // Press Space to select and register folder
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char(' ')))
        .await;

    assert!(!app.state.show_folder_picker_modal);
    assert_eq!(app.state.folder, "/home/user/workspace/ferry-project");
}

#[tokio::test]
async fn test_tuiapp_folder_picker_filter_and_selection_enter() {
    let fake = Arc::new(FakeBackend::new());
    let trait_backend: Arc<dyn UiBackend> = fake.clone();
    let mut app = TuiApp::new_with_backend(trait_backend.clone());

    app.state.folder_picker.set_listing(sample_listing());
    app.state.show_folder_picker_modal = true;

    // Type 'notes' to filter down to notes.txt
    for c in "notes".chars() {
        app.handle_key_action(&trait_backend, make_key(KeyCode::Char(c)))
            .await;
    }
    assert_eq!(app.state.folder_picker.filtered_items().len(), 1);

    // Press Space to select
    app.handle_key_action(&trait_backend, make_key(KeyCode::Char(' ')))
        .await;

    assert!(!app.state.show_folder_picker_modal);
    assert_eq!(app.state.folder, "/home/user/workspace/notes.txt");
}

#[tokio::test]
async fn test_tuiapp_folder_picker_traversal_enter_directory() {
    let fake = Arc::new(FakeBackend::new());
    let trait_backend: Arc<dyn UiBackend> = fake.clone();
    let mut app = TuiApp::new_with_backend(trait_backend.clone());

    app.state.folder_picker.set_listing(sample_listing());
    app.state.show_folder_picker_modal = true;

    // Navigate to "ferry-project" (index 1)
    app.handle_key_action(&trait_backend, make_key(KeyCode::Down))
        .await;
    assert_eq!(app.state.folder_picker.selected_index, 1);

    // Press Enter to enter directory
    app.handle_key_action(&trait_backend, make_key(KeyCode::Enter))
        .await;

    // In FakeBackend, listing a non-existing path returns mock project-a / project-b entries for target
    assert!(app.state.show_folder_picker_modal);
    assert_eq!(
        app.state.folder_picker.current_path,
        PathBuf::from("/home/user/workspace/ferry-project")
    );
}

#[tokio::test]
async fn test_tuiapp_folder_picker_traversal_enter_parent() {
    let fake = Arc::new(FakeBackend::new());
    let trait_backend: Arc<dyn UiBackend> = fake.clone();
    let mut app = TuiApp::new_with_backend(trait_backend.clone());

    app.state.folder_picker.set_listing(sample_listing());
    app.state.show_folder_picker_modal = true;
    assert_eq!(app.state.folder_picker.selected_index, 0); // on ..

    // Press Enter on .. to navigate up
    app.handle_key_action(&trait_backend, make_key(KeyCode::Enter))
        .await;

    assert!(app.state.show_folder_picker_modal);
    assert_eq!(
        app.state.folder_picker.current_path,
        PathBuf::from("/home/user")
    );
}

#[test]
fn test_tuiapp_handle_key_legacy_ipc_commands() {
    let mut app = TuiApp::default();
    app.state.folder = "/test/folder".to_string();

    // 'A' opens modal and returns ListDirectory command
    let cmd = app.handle_key(make_key(KeyCode::Char('A')));
    assert_eq!(
        cmd,
        Some(ClientCommand::ListDirectory {
            path: Some("/test/folder".to_string())
        })
    );
    assert!(app.state.show_folder_picker_modal);

    // Populate listing
    app.state.folder_picker.set_listing(sample_listing());

    // Navigate down to ferry-project
    app.handle_key(make_key(KeyCode::Down));

    // Space registers folder
    let cmd_reg = app.handle_key(make_key(KeyCode::Char(' ')));
    assert_eq!(
        cmd_reg,
        Some(ClientCommand::RegisterFolder {
            path: "/home/user/workspace/ferry-project".to_string()
        })
    );
    assert!(!app.state.show_folder_picker_modal);
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
fn test_render_folder_picker_modal() {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut state = TuiState::default();
    state.folder_picker.set_listing(sample_listing());
    state.show_folder_picker_modal = true;
    let app = TuiApp::new(state);

    terminal.draw(|f| app.render(f)).unwrap();

    let rendered = buffer_to_string(terminal.backend());
    assert!(
        rendered.contains("Filesystem Explorer"),
        "Missing explorer title"
    );
    assert!(
        rendered.contains("/home/user/workspace"),
        "Missing current path"
    );
    assert!(
        rendered.contains("ferry-project"),
        "Missing ferry-project folder"
    );
    assert!(rendered.contains("[git]"), "Missing [git] badge");
    assert!(rendered.contains("[synced]"), "Missing [synced] badge");
    assert!(rendered.contains("[link]"), "Missing [link] badge");
    assert!(
        rendered.contains(".. (parent directory)"),
        "Missing parent navigation item"
    );
    assert!(rendered.contains("📁"), "Missing folder icon");
}
