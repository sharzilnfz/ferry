# 05: TUI interactive actions and keybinds

**What to build:** Add interactive keybindings to the TUI (`p` to toggle session pinning, `r` to trigger a manual scan, `c` to open the conflict inspector, `q` or `Esc` to quit) that send `ClientCommand` messages over IPC and instantly reflect responses in the terminal view.

**Blocked by:** 02: Daemon IPC server and engine broadcast, 03: ferry-tui core layout and test backend.

**Status:** closed

## Implementation Notes for Agent
- Use `codebase-memory-mcp` to inspect `crates/ferry-tui` and `crates/ferry-ipc`.
- Implement key event handlers in `TuiApp`:
  - `q` / `Ctrl+C`: restores terminal settings, clears alternate screen, exits process cleanly.
  - `p`: sends `ClientCommand::StartPin` if unpinned, or `ClientCommand::ReleasePin` if active.
  - `r`: sends `ClientCommand::TriggerScan`.
  - `c`: toggles full-screen conflict detail modal showing quarantined filenames and conflict timestamps.
- Ensure terminal state (raw mode, alternate screen, mouse capture) is reliably restored even on panics or error returns.

## Acceptance Criteria
- [x] Pressing `q` or `Esc` exits the TUI and cleanly restores normal terminal mode.
- [x] Pressing `p` toggles pin state over IPC, updating the header badge from `SYNCED` to `PINNED`.
- [x] Pressing `c` displays quarantined conflict entries from `.ferry/conflicts.jsonl`.
- [x] `TestBackend` tests verify all keypress actions and modal state transitions.
