# Ferry Terminal User Interface (TUI): Architecture, Controls & Dual-Machine Testing Guide

This guide explains how the Ferry TUI dashboard works, its internal architecture, available keybindings, and how to test synchronization across two machines entirely from the terminal.

---

## 1. Overview

The Ferry TUI is a high-performance, event-driven terminal dashboard built directly into the `ferry` binary. It gives developers a real-time, zero-CPU overview of synchronization health, storage deduplication metrics, peer connections, and conflict states without opening a web browser.

The TUI is implemented in Rust using `ratatui` and `crossterm`. It connects to the local Ferry daemon over IPC sockets via `AutoBackend`. If no daemon is running, it operates directly against local disk stores in standalone mode.

---

## 2. Current Implementation Status

| Component | Status | Location | Notes |
|---|---|---|---|
| **TUI Application Loop** | Production-Ready | `crates/ferry-tui/src/app.rs` | Handles keyboard input, push event dispatch, and frame rendering. |
| **Ratatui Layout Engine** | Production-Ready | `crates/ferry-tui/src/ui.rs` | 4-part split layout with responsive sizing down to 40x12 terminals. |
| **CLI Runner Seam** | Production-Ready | `crates/ferry-cli/src/commands/tui.rs` | Dispatched via `ferry tui` or `ferry ui --tui`. |
| **Terminal Guard & Panic Hook** | Production-Ready | `crates/ferry-tui/src/terminal.rs` | Restores raw terminal modes cleanly even if an uncaught panic occurs. |
| **Folder Picker Modal** | Production-Ready | `crates/ferry-tui/src/picker.rs` | In-terminal interactive directory navigator with fuzzy filtering. |
| **Conflicts Modal** | Production-Ready | `crates/ferry-tui/src/ui.rs:550` | Full-screen modal displaying quarantined loser copies. |
| **Live Transfer Gauge** | Production-Ready | `crates/ferry-tui/src/ui.rs:250` | Progress bar rendering chunk transfers, ETA, and throughput. |

---

## 3. Architecture & Runtime Flow

```
+-------------------------------------------------------------------+
|                        Terminal Display                           |
|       (Ratatui 0.29 / Crossterm 0.28 / Alternate Screen Buffer)   |
+-------------------------------------------------------------------+
             │                                        ▲
       Raw Key Events                        Redraw Frame (30 FPS)
     (crossterm::event)                      (TuiState Snapshots)
             │                                        │
             ▼                                        │
+-------------------------------------------------------------------+
|                          TuiApp Loop                              |
|               (crates/ferry-tui/src/app.rs)                       |
+-------------------------------------------------------------------+
             │                                        ▲
     ClientCommand Actions                   UiEvent Push Stream
             │                                        │
             ▼                                        │
+-------------------------------------------------------------------+
|               AutoBackend (crates/ferry-ipc)                      |
|      (connect_auto with Daemon Socket & In-Process Fallback)      |
+-------------------------------------------------------------------+
```

### Dashboard Layout Structure
The dashboard splits the terminal window into four clear regions:
1. **Header Panel:** Displays folder root path, unique folder ID, local device ID, manifest hash, and live engine status badge (`SYNCED`, `SYNCING`, `PINNED`, `CONFLICT`, `OFFLINE`).
2. **Main Left Box (Storage & Progress):** Shows stored blob counts, total data size, deduplication ratio, and active chunk transfer gauges.
3. **Main Right Box (Peer Connectivity):** Renders a table of paired peers, showing peer device IDs, transport routes (LAN direct or relay), latency, agreement status, and last-seen timestamps.
4. **Activity Log Panel:** Displays a rolling, timestamped log of sync events, scan completions, and peer handshakes with color-coded severity.
5. **Footer Bar:** Renders active hotkey reminders.

---

## 4. Keybindings and Controls

| Keybinding | Action | Description |
|---|---|---|
| `q` or `Esc` | **Quit** | Exits the TUI cleanly and restores standard terminal mode. |
| `Ctrl + C` | **Force Exit** | Immediately terminates the application. |
| `r` or `R` | **Rescan** | Triggers an immediate incremental scan and delta publication. |
| `p` or `P` | **Toggle Pin** | Holds edits during agent sessions or releases pending changes. |
| `c` or `C` | **Conflicts** | Opens the conflict quarantine modal. Press `Esc` or `q` to dismiss. |
| `a` or `o` | **Add Folder** | Opens the interactive directory picker modal to register folders. |
| `Space` *(in picker)* | **Select** | Validates and registers the highlighted folder into sync. |
| `Enter` *(in picker)* | **Navigate** | Drills down into the highlighted directory. |
| `Up` / `Down` | **Navigate** | Moves selection across lists and tables. |

---

## 5. Step-by-Step Dual-Machine Testing Guide (For Tomorrow)

### Build Binaries
Ensure the release binary is built on both machines:
```bash
cargo build --release
```

---

### Step 1: Initialize and Pair via CLI

1. **On Machine A (Offering Device):**
   ```bash
   cd ~/my-sync-folder
   target/release/ferry init
   target/release/ferry share
   ```
   *Note the 6-character code (e.g. `K9X2M4`).*

2. **On Machine B (Joining Device):**
   ```bash
   mkdir -p ~/my-sync-folder && cd ~/my-sync-folder
   target/release/ferry join K9X2M4
   ```

---

### Step 2: Launch TUI Dashboards on Both Machines

1. **On Machine A:**
   ```bash
   cd ~/my-sync-folder
   target/release/ferry tui
   ```

2. **On Machine B:**
   ```bash
   cd ~/my-sync-folder
   target/release/ferry tui
   ```

Both terminals will render the live Ferry dashboard side by side.

---

### Step 3: Run Verification Scenarios

1. **Observe Peer Discovery:**
   - Within seconds, both screens show each other in the Peers table on the right.
   - The status badge in the header turns green (`SYNCED`).

2. **Test Real-Time File Sync:**
   - In a third terminal window on Machine A, create a test file:
     ```bash
     echo "Testing terminal sync" > ~/my-sync-folder/terminal-test.txt
     ```
   - Watch the TUI on Machine B. The Transfer Progress bar will flash, the activity log records the received chunks, and the status badge transitions smoothly.

3. **Test Session Pinning (Agent Hold Mode):**
   - On Machine A, press `p`.
   - The status badge changes to magenta (`PINNED`), and the activity log announces edit hold mode.
   - In another terminal on Machine B, modify a file:
     ```bash
     echo "Remote edit" >> ~/my-sync-folder/terminal-test.txt
     ```
   - Machine A holds the incoming change in `.ferry/held/` without overwriting the local working tree.
   - Press `p` again on Machine A. The engine releases and reconciles the held changes immediately.

4. **Inspect Conflicts Modal:**
   - Trigger a concurrent edit on both sides.
   - The status badge turns yellow-on-red (`CONFLICT`).
   - Press `c` on either machine to open the Conflicts inspection popup.
   - Review the loser copy and quarantine details, then press `Esc` to return to the main dashboard.

---

## 6. Where Things Live

- TUI Application State & Event Loop: `crates/ferry-tui/src/app.rs`
- Ratatui Layout & Widget Rendering: `crates/ferry-tui/src/ui.rs`
- State Containers & Metrics Caches: `crates/ferry-tui/src/state.rs`
- Activity Logging Buffer: `crates/ferry-tui/src/activity_log.rs`
- Terminal Guard & Raw Mode Lifecycles: `crates/ferry-tui/src/terminal.rs`
- Directory Picker: `crates/ferry-tui/src/picker.rs`
- CLI Entry Seam: `crates/ferry-cli/src/commands/tui.rs`

---

## 7. Gotchas & Troubleshooting

1. **Terminal Resize Limits:** The dashboard requires at least a 40x12 terminal size. Resizing below this threshold displays a clean warning message rather than panicking.
2. **Raw Mode Cleanups:** If the terminal process receives an unexpected termination signal, `TerminalGuard` automatically restores normal terminal echo and cursor visibility.
3. **Daemon Detection:** If the Ferry daemon is not running in the background, the TUI transparently connects directly to disk stores. Starting `ferry daemon` in another pane automatically switches the TUI to socket event multiplexing.
