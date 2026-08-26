# 03: ferry-tui core layout and test backend

**What to build:** An event-driven terminal dashboard built with `ratatui` and `crossterm` that receives `DaemonMessage` events over IPC, updates local UI state, and renders the double-buffered status header, storage metrics, peer connectivity table, and recent activity log with zero CPU consumption at idle.

**Blocked by:** 01: ferry-ipc crate and wire protocol.

**Status:** ready-for-agent

## Implementation Notes for Agent
- Use `codebase-memory-mcp` to inspect `docs/cli-json.md` and `crates/ferry-daemon/src/ui/status.rs` for field formatting conventions.
- Implement the `TuiApp` state machine and `ratatui` widgets:
  - Header: Folder path, Device ID, Folder ID, Engine State badge (`SYNCED`, `SYNCING`, `CONFLICT`, `PINNED`).
  - Left pane: Scanned files/dirs, Manifest hash, Pin status, Transfer progress gauge.
  - Right pane: Peers list (Device IDs, reachability status, last agreed manifest, latency).
  - Bottom pane: Recent activity log.
  - Footer: Hotkey bar.
- Use `ratatui::backend::TestBackend` to build headless unit and regression tests without requiring a real terminal.
- Ensure event loop sleeps on `tokio::select!` and wakes only on incoming IPC messages or terminal key events.

## Acceptance Criteria
- [ ] New crate `crates/ferry-tui` compiles cleanly.
- [ ] `TestBackend` tests verify rendering against 80x24 and 120x40 character grids for all sync states (`SYNCED`, `SYNCING`, `CONFLICT`, `PINNED`).
- [ ] Progress gauge renders chunk transfer progress smoothly without string allocations inside the render loop.
- [ ] Activity log records incoming events and truncates to a fixed circular buffer.
