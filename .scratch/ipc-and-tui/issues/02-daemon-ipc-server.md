# 02: Daemon IPC server and engine broadcast

**What to build:** Integrate the IPC server into `ferry-daemon` so the running sync engine pushes live snapshots, state transitions, transfer metrics, and conflict alerts to local clients. Remove the always-on Axum web server and embedded static assets from default daemon startup.

**Blocked by:** 01: ferry-ipc crate and wire protocol.

**Status:** ready-for-agent

## Implementation Notes for Agent
- Use `codebase-memory-mcp` to inspect `crates/ferry-sync/src/engine.rs` and `crates/ferry-daemon/src/main.rs`.
- Connect an IPC server task to the engine state changes, broadcasting `DaemonMessage::Snapshot`, `DaemonMessage::StateChanged`, `DaemonMessage::TransferProgress`, and `DaemonMessage::ConflictRecorded`.
- Handle client requests: `ClientCommand::GetStatus`, `ClientCommand::StartPin`, `ClientCommand::ReleasePin`, `ClientCommand::TriggerScan`.
- Remove default `--ui` Axum listener spawning from `ferry-daemon/src/main.rs`.
- Clean up socket files on graceful exit.

## Acceptance Criteria
- [ ] Daemon starts headlessly and opens the local IPC socket.
- [ ] Connecting clients receive an immediate `DaemonMessage::Snapshot` with full current folder state.
- [ ] Engine state changes emit `DaemonMessage::StateChanged` events across all active IPC client streams.
- [ ] `ClientCommand::StartPin` and `ClientCommand::ReleasePin` alter the daemon engine pin state immediately.
- [ ] Terminating the daemon cleans up the socket file from disk.
