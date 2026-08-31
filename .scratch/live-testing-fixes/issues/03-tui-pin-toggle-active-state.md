# Issue 3: Fix TUI pin toggle to release active pin when holding is false

Status: ready-for-agent
Feature: `live-testing-fixes`
Depends on: .scratch/live-testing-fixes/issues/02-short-code-pairing-rendezvous.md
Blocks: .scratch/live-testing-fixes/issues/04-tui-backend-disconnected-handling.md

## Context
In `crates/ferry-tui/src/app.rs`, the `P` key shortcut toggles session pinning by checking:
```rust
if self.state.pin.holding || self.state.engine_state == SyncState::Pinned {
    KeyOutcome::Command(ClientCommand::ReleasePin)
} else {
    KeyOutcome::Command(ClientCommand::StartPin { ... })
}
```
When a pin is started without active competing remote writes in progress, `self.state.pin.holding` is `false` and `engine_state` is `SyncState::Idle` (while `self.state.pin.state` is `"active"` or `"pinned"`). Pressing `P` a second time attempts to invoke `StartPin` again instead of releasing or stopping the active pin, causing an error log:
`[ERR ] Start pin error: pin-active: a pin is already active on this folder`.

## Target Files
- `crates/ferry-tui/src/app.rs`
- `crates/ferry-tui/src/state.rs`
- `crates/ferry-tui/tests/key_event_tests.rs`

## Requirements
1. Update `ferry-tui::app::handle_key_inner` for `KeyCode::Char('p' | 'P')` to check if a pin is active (e.g. `self.state.pin.state != "none"` or `self.state.pin.is_active()`).
2. When active, dispatch `ClientCommand::ReleasePin` or stop the pin, properly toggling the state back to `none`.
3. Add a unit test in `crates/ferry-tui/tests/key_event_tests.rs` verifying that pressing `P` when `pin.state == "active"` and `pin.holding == false` generates `ClientCommand::ReleasePin`.
