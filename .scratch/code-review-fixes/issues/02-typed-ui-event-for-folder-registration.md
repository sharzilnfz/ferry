# 02: Typed UIEvent variant for folder registration

**What to build:** Discontinue the misuse of `UiEvent::Error` with magic code `"folder_registered"` for folder registration success in the GUI. Introduce a typed event or clean channel in `ferry-ipc` / `ferry-gui` so error channels are strictly reserved for genuine failures and success lifecycle events are typed.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] `UiEvent` or the GUI backend action response channel uses a typed success event rather than `UiEvent::Error { code: "folder_registered", .. }`
- [ ] `ferry-gui` handles the typed success event to update its folder list and activity status
- [ ] Error dispatch across GUI and backend remains strictly for error conditions
- [ ] GUI tests in `crates/ferry-gui/tests/gui_tests.rs` pass with the typed event
