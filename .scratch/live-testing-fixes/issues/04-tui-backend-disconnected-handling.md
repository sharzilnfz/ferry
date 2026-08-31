# Issue 4: Prevent TUI activity log spam when daemon is disconnected

Status: ready-for-agent
Feature: `live-testing-fixes`
Depends on: .scratch/live-testing-fixes/issues/03-tui-pin-toggle-active-state.md
Blocks: .scratch/live-testing-fixes/issues/05-cli-web-token-query-command.md

## Context
When running `ferry tui <folder>` without an active daemon process, the IPC/backend event stream connection fails or closes immediately. The TUI event loop enters a fast retry/reconnect cycle that logs:
`[ERR ] Backend event stream closed`
multiple times per second, filling up the Recent Activity feed and burying actual user logs.

## Target Files
- `crates/ferry-tui/src/app.rs`
- `crates/ferry-tui/src/ui.rs`
- `crates/ferry-tui/src/state.rs`

## Requirements
1. In `ferry-tui::app::run` or the backend event listener loop, add an exponential backoff / reconnect cooldown (e.g. 1s -> 2s -> 5s) upon stream closure.
2. Deduplicate or suppress consecutive `Backend event stream closed` error entries from the visual activity feed.
3. Show an explicit `DISCONNECTED` or `DAEMON OFFLINE` indicator on the TUI top banner when the backend is unreachable.
4. Add automated test coverage verifying stream reconnect throttle behavior and activity log deduplication.
