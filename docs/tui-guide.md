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

## 5. Step-by-Step Dual-Machine Testing Guide

### Build Binaries
Build the debug or release binary on both machines:
```bash
cargo build -p ferry-cli              # Fast debug build -> ./target/debug/ferry
# Or: cargo build --release -p ferry-cli -> ./target/release/ferry
```

---

### Step 1: Initialize and Pair via Offer Flow

> **Pairing note:** Use `ferry pair` and `ferry pair --accept` (the offer-file flow). The short-code `share`/`join` path does not complete key-wrap allow-lists on the sharer in this build (issue T-016).

1. **On Machine A (e.g. Mac):**
   ```bash
   mkdir -p /tmp/ferry-sync-demo && cd /tmp/ferry-sync-demo
   ./target/debug/ferry init
   ./target/debug/ferry pair
   ```
   *Leaves offer file at `.ferry/pair-offer.ferry-pair` and waits for response.*

2. **On Machine B (e.g. Arch Linux):**
   ```bash
   mkdir -p /tmp/ferry-sync-demo && cd /tmp/ferry-sync-demo
   # Copy offer file from Machine A (via scp):
   scp mac:/tmp/ferry-sync-demo/.ferry/pair-offer.ferry-pair /tmp/pair-offer.ferry-pair
   ./target/debug/ferry pair --accept /tmp/pair-offer.ferry-pair /tmp/ferry-sync-demo
   ```
   *Writes `/tmp/pair-response.ferry-pair`.*

3. **Complete the 2-way roundtrip:**
   - Copy response from Machine B to Machine A:
     ```bash
     scp arch:/tmp/pair-response.ferry-pair /tmp/ferry-sync-demo/.ferry/pair-response.ferry-pair
     ```
     *(Machine A completes and creates `.ferry/pair-grant.ferry-grant`)*
   - Copy grant from Machine A to Machine B:
     ```bash
     scp /tmp/ferry-sync-demo/.ferry/pair-grant.ferry-grant arch:/tmp/pair-grant.ferry-grant
     ```
     *(Machine B completes)*

---

### Step 2: Start Background Daemons and Launch TUI

1. **On Machine A (Listener):**
   ```bash
   ./target/debug/ferry daemon --listen 0.0.0.0:44001 /tmp/ferry-sync-demo
   ```

2. **On Machine B (Dialer in background + TUI in foreground):**
   ```bash
   nohup ./target/debug/ferry daemon --peer-url <MACHINE_A_IP>:44001 --interval-secs 1 /tmp/ferry-sync-demo > /tmp/ferry-daemon.log 2>&1 &
   ./target/debug/ferry tui /tmp/ferry-sync-demo
   ```

---

### Step 3: Run Verification Scenarios

1. **Observe Peer Discovery:**
   - Within a second, the TUI shows the connected peer in the Connected Peers table.
   - The status badge in the header displays `IDLE` (synced and listening).

2. **Test Real-Time File Sync:**
   - On Machine A, create a test file:
     ```bash
     echo "Testing terminal sync" > /tmp/ferry-sync-demo/terminal-test.txt
     ```
   - Watch the TUI on Machine B: the scan updates, chunk counts increment, and the activity log shows updated manifest hashes.

3. **Test In-TUI Rescan & Modals:**
   - Press **`R`**: Activity log immediately records `[INFO] Scan triggered`.
   - Press **`C`**: Opens the Quarantined Conflicts modal. Press `Esc` or `C` to return.
   - Press **`P`**: Toggles session hold/pinning.
   - Press **`Q`**: Closes TUI cleanly.

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
