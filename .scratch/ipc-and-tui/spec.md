# Status: ready-for-agent

# Specification: Headless Daemon IPC, Optimized TUI, and On-Demand Web UI

## Problem Statement

Ferry currently bundles an HTTP server and web dashboard directly into the background sync daemon. This design introduces several concrete issues for developers and coding agents:

1. Running an always-on web server consumes memory, thread pool capacity, and network ports even when no user looks at a dashboard.
2. Web dashboards rely on periodic HTTP polling (2-second intervals), causing unnecessary CPU wakeups, state file re-reads, and JSON serialization.
3. Terminal-first developers must switch context out of their editor or shell to a browser tab just to inspect sync status, peer connectivity, or active session pins.
4. An open, unauthenticated localhost HTTP port expands the local attack surface on multi-user machines or against browser scripts running in other tabs.
5. The CLI and Web dashboard duplicate logic for reading and parsing `.ferry/` internal state files instead of querying a single authoritative daemon state.

Developers need an ambient, lightweight, zero-idle-CPU sync experience in their terminal, with visual surfaces available strictly on demand.

## Solution

Decouple the sync engine from HTTP networking by introducing a clean three-part architecture:

1. **Headless background daemon**: The core sync daemon runs without any web server. It handles content-defined chunking, QUIC transport, and tree reconciliation. It exposes an authoritative local IPC socket using newline-delimited JSON over a Unix domain socket on macOS and Linux, or a named pipe on Windows.
2. **High-performance event-driven TUI**: A terminal interface built with `ratatui` and `crossterm` (`ferry tui` or `ferry status -w`). It connects to the local IPC socket, updates purely on push events, renders within an alternate-screen buffer, and consumes 0.0% CPU when idle.
3. **On-demand ephemeral web UI**: An interactive browser dashboard launched explicitly via `ferry ui`. The CLI starts a temporary local web server on a random port, opens the default browser with a one-time token, proxies requests to the daemon IPC socket, and shuts down automatically when inactive or closed.
4. **Unified CLI integration**: Standard CLI commands (`ferry status`, `ferry conflicts`, `ferry pin`) query the daemon IPC socket when the daemon is running, getting instant in-memory responses without scanning the disk.

---

## User Stories

1. As a terminal-focused developer, I want the sync daemon to run headless without opening TCP ports, so that it uses minimal RAM and CPU while syncing in the background.
2. As a developer monitoring long-running agent work, I want a live terminal dashboard (`ferry tui`), so that I can see sync progress in real time without opening a browser.
3. As a developer with battery constraints on a laptop, I want the TUI to sleep when no events arrive, so that monitoring does not drain my battery.
4. As a developer running an AI agent overnight, I want the daemon to process file writes without web server overhead or browser polling interference.
5. As a developer encountering concurrent edit conflicts, I want the TUI to alert me immediately in red, so that I can inspect quarantined files before proceeding.
6. As a developer working across multiple machines, I want to see a clear list of paired peers, their direct/relay transport links, and last agreement times in the TUI.
7. As a developer sharing a folder for the first time, I want to run `ferry ui` to generate and display an on-demand QR code in my browser for quick mobile or laptop pairing.
8. As a developer resolving complex merge conflicts, I want to launch `ferry ui` to view side-by-side visual diffs, knowing the web server will terminate when I close the tab.
9. As a developer scripting health checks, I want `ferry status --json` to fetch the cached daemon state instantly over IPC, avoiding redundant tree scans.
10. As a developer on a shared machine, I want the daemon IPC to use filesystem permissions on a Unix domain socket, so that other local users cannot query my synced folders.
11. As a developer using session pinning, I want to press `p` in the TUI to immediately pin my folder against remote writes while editing locally.
12. As a developer releasing a session pin, I want to press `p` again in the TUI, so that remote changes resume flowing automatically.
13. As a developer diagnosing network issues, I want the TUI to display direct vs relay status and ping latency for each peer.
14. As a developer with a slow terminal, I want the TUI to throttle redraws during window resizing, so that resizing remains smooth without artifact glitching.
15. As a developer running tests in CI, I want the TUI components to support headless testing with in-memory buffers, so that regression tests run fast without real terminal windows.
16. As an AI agent checking sync status, I want to query the IPC socket directly with newline-delimited JSON commands, receiving structured state objects without parsing human CLI text.
17. As a developer stopping the daemon, I want the IPC socket file to be cleaned up cleanly on exit, leaving no stale socket files behind.
18. As a developer running `ferry status` when the daemon is stopped, I want the CLI to fall back gracefully to a direct disk read of the store and `.ferry/` files.

---

## Implementation Decisions

### 1. New Crates and Module Boundaries

- **`crates/ferry-ipc`**:
  - Encapsulates the local IPC wire protocol, client connector, and server listener.
  - Platform backends: `tokio::net::UnixListener` / `UnixStream` on Unix platforms (`~/.ferry/daemon.sock` or `<store>/.ferry/daemon.sock`); `tokio::net::windows::named_pipe` on Windows (`\\.\pipe\ferry-<folder_id>`).
  - Wire framing: Newline-delimited JSON messages (`\n`).
  - Types: `DaemonMessage` (server push) and `ClientCommand` (client request).

- **`crates/ferry-tui`**:
  - Implements the interactive terminal dashboard using `ratatui` (0.28+) and `crossterm` (0.28+).
  - State management: A single `TuiState` struct updated purely by incoming `DaemonMessage` events and terminal keystrokes.
  - Render loop: `tokio::select!` over IPC incoming channel and terminal event stream. No 60 FPS busy loop.

- **`crates/ferry-daemon` modifications**:
  - Remove default Axum web server and embedded static assets.
  - Attach the `ferry-ipc` server task to the `SyncEngine` event broadcast channel.
  - Broadcast `DaemonMessage::Snapshot`, `DaemonMessage::StateChanged`, `DaemonMessage::TransferProgress`, and `DaemonMessage::ConflictRecorded`.

- **`crates/ferry-cli` modifications**:
  - Add `ferry tui` command (and alias `ferry status --watch` / `ferry status -w`).
  - Add `ferry ui` command: an ephemeral Axum server spawned on `127.0.0.1:0` with an activity watchdog timer (10-minute idle timeout).
  - Update `ferry status`, `ferry conflicts`, `ferry pin` to attempt IPC socket queries first, falling back to direct store reads if the daemon is offline.

### 2. IPC Message Contracts

```rust
// In crates/ferry-ipc/src/protocol.rs

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum DaemonMessage {
    Snapshot(EngineSnapshot),
    StateChanged {
        state: String,
        manifest_id: String,
        agreed_id: Option<String>,
    },
    TransferProgress {
        bytes_transferred: u64,
        total_bytes: u64,
        current_path: String,
    },
    ConflictRecorded {
        path: String,
        conflict_path: String,
        timestamp: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "command", content = "args")]
pub enum ClientCommand {
    GetStatus,
    StartPin { paths: Vec<String> },
    ReleasePin,
    TriggerScan,
    ListConflicts,
}
```

### 3. TUI Layout and Performance Constraints

- **Layout Structure**:
  - Top header (3 lines): Folder path, Folder ID, Device ID, Engine State badge (`SYNCED`, `SYNCING`, `CONFLICT`, `PINNED`).
  - Main body (split 55% / 45%):
    - Left column: Tree metrics (files, dirs, size, chunked bytes), Manifest hash, Active pin status, and Transfer progress bar.
    - Right column: Peer list table (Device ID, Transport direct/relay, Last agreed manifest, Latency/status).
  - Bottom pane (flexible height): Recent activity log (scans, blob transfers, errors).
  - Footer (1 line): Hotkey guide (`[q] Quit`, `[p] Pin`, `[r] Rescan`, `[c] Conflicts`, `[w] Open Web UI`).

- **Performance Rules**:
  - Zero allocation during rendering: strings, hashes, and formats are prepared in state updates, not re-allocated inside `terminal.draw`.
  - Frame rendering occurs only when an IPC event or keyboard input is processed.
  - Window resize events are throttled with a 16ms debouncer to avoid terminal tearing.

---

## Testing Decisions

### What Makes a Good Test

Tests must verify externally observable behavior and protocol contracts rather than private internal implementation details. Tests must run deterministically, without relying on real terminal displays or fixed network ports.

### Primary Testing Seams

1. **The IPC Protocol Seam (`crates/ferry-ipc`)**:
   - Test both Unix domain sockets and Windows named pipes using in-memory duplex streams (`tokio::io::duplex`).
   - Verify serialization and deserialization of all `DaemonMessage` and `ClientCommand` variants.
   - Verify server handling of unexpected client disconnections, invalid JSON, and rapid reconnects.

2. **The TUI State and Render Seam (`crates/ferry-tui`)**:
   - Use `ratatui::backend::TestBackend` with fixed terminal dimensions (80x24 and 120x40).
   - Feed synthetic `DaemonMessage` sequences (initial snapshot, state change, chunk transfer, conflict record) into `TuiApp`.
   - Assert exact character buffers, color styles, and widget positions using snapshot assertions (`assert_buffer`).
   - Test keyboard event handling (`q` exits loop, `p` sends `StartPin` command over IPC).

3. **Daemon-to-Client End-to-End Seam (`crates/ferry-daemon` + `crates/ferry-cli`)**:
   - Integration test spawning a real `SyncEngine` with loopback transport and an IPC listener.
   - Run CLI status and TUI client against the socket, verifying that:
     - The daemon emits a valid `EngineSnapshot` immediately upon client connection.
     - Tree changes trigger `StateChanged` messages over IPC within 50ms.
     - Pin commands issued via IPC take immediate effect in the engine.
   - Verify graceful fallback: if the IPC socket is absent, `ferry status` falls back to reading the store directly.

4. **Performance and Idle Resource Benchmarks**:
   - Benchmark memory RSS of the headless daemon versus the old web-bundled daemon.
   - Verify CPU usage remains below 0.1% during 60 seconds of idle TUI connection.

### Prior Art in Codebase

- Store format and diffing tests in `crates/ferry-store/tests/`.
- CLI JSON schema compliance tests in `crates/ferry-cli/tests/cli_parse.rs` and `expected/*.schema.txt`.
- Engine loopback synchronization tests in `crates/ferry-cli/tests/exchange_loopback.rs`.

---

## Out of Scope

- Remote network IPC (IPC is strictly local to the machine via filesystem permissions or named pipes).
- Authentication tokens on the local IPC socket (relying on OS user account permissions).
- Third-party GUI frameworks or desktop electron wrappers.
- Interactive multi-pane file diff editors inside the TUI (file diffs are handled by external tools or the ephemeral Web UI).

---

## Further Notes

- Once this specification is implemented, the permanent `--ui` argument in `ferry daemon` can be deprecated in favor of `ferry ui`.
- The local IPC mechanism also establishes the foundation for future editor extensions (such as a VS Code status bar item) to query the daemon with zero overhead.
