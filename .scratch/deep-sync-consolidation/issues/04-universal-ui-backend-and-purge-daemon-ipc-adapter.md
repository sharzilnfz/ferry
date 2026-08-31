# 04: Universal UI Backend & Purge DaemonIpcAdapter

**What to build:** Delete the redundant pass-through `DaemonIpcAdapter` from `ferry-daemon::ui::backend`. Move `AutoBackend` into `ferry-ipc::backend` as the single universal adapter providing automated IPC reconnection and local fallback across GUI, TUI, and CLI.

**Status:** ready-for-agent

**Depends on:** None

**Blocks:** None

- [ ] Delete `DaemonIpcAdapter` from `crates/ferry-daemon/src/ui/backend.rs`
- [ ] Move `AutoBackend` into `crates/ferry-ipc/src/backend.rs` implementing `UiBackend`
- [ ] Provide unified factory `ferry_ipc::backend::connect_auto(socket_path, folder_path)`
- [ ] Update `ferry-gui` and `ferry-tui` to consume `connect_auto` directly
- [ ] Verify GUI, TUI, and IPC contract tests pass with 0 failures
