# 01: Canonical uninitialized folder domain error in ferry-folder

**What to build:** Centralize the uninitialized folder validation error and remedy hint in `ferry-folder`. Eliminate hardcoded duplicate error and hint strings across TUI (`picker.rs`), GUI (`app.rs`), and Web UI server (`server.rs`). All surfaces must derive their user-facing error message and remedy hint from `ferry_folder::FolderError`.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] `ferry-folder` exports a canonical `FolderError::not_initialized(path: &Path)` constructor or constant with standard message and `"run 'ferry init' or 'ferry pair' before syncing this folder"` hint
- [ ] TUI picker uses the canonical error definition instead of local hardcoded constants
- [ ] GUI folder registration action uses the canonical error definition
- [ ] Web UI endpoint `/api/registry/register` returns the canonical error message and hint
- [ ] Existing unit and integration tests across TUI, GUI, and Web UI continue to pass
