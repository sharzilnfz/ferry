# 01: Interactive Filesystem Explorer in Terminal TUI

**What to build:** An interactive filesystem browser inside the Terminal TUI. Users press `A` or `O` to open a directory navigation modal, browse folders with arrow keys and `Enter`, filter paths by typing, and press `Space` to select a folder for sync.

**Blocked by:** None (can start immediately).

**Status:** closed

- [x] `UiBackend` trait and `ferry-ipc` support `list_directory(path)` returning path metadata (directory, git repository, already synced).
- [x] TUI renders a modal filesystem tree with folder icons, parent navigation (`..`), and live search filtering.
- [x] Keyboard shortcuts `A` and `O` open the modal; `Space` or `Enter` selects the highlighted folder.
- [x] Unit and component tests verify keyboard navigation and selection against `FakeBackend`.
