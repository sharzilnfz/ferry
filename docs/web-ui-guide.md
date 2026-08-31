# Ferry Web UI: Architecture, Setup & Dual-Machine Testing Guide

This guide explains how the Ferry Web UI works, its current implementation status, and how to test synchronization across two machines.

---

## 1. Overview

The Ferry Web UI is a standalone, local-first web dashboard embedded directly inside the `ferry` CLI binary. It provides a visual interface for pairing devices, monitoring synchronization health, holding edits during AI agent runs, and inspecting conflicts.

The frontend is built as a single-page application using vanilla modern JavaScript and custom CSS. It connects to an embedded Axum HTTP server backed by `AutoBackend`, communicating via JSON REST endpoints and a real-time Server-Sent Events (SSE) stream.

---

## 2. Current Implementation Status

| Component | Status | Location | Notes |
|---|---|---|---|
| **Axum Web Server** | Production-Ready | `crates/ferry-daemon/src/ui/server.rs` | Serves embedded assets, REST endpoints, and SSE stream. |
| **CLI Dispatcher** | Production-Ready | `crates/ferry-cli/src/commands/ui.rs` | Handles `ferry ui --web` with port selection and browser auto-open. |
| **Token Authentication** | Production-Ready | `server.rs:188` & `app.js:105` | 32-character random hex token with constant-time validation. |
| **Real-Time Streaming** | Production-Ready | `server.rs:109` (`/api/events`) | Live SSE broadcast for state transitions and conflict events. |
| **Pairing Ritual UI** | Production-Ready | `app.js:800-950` | Generates 6-char Base32 codes, renders QR codes, and joins peers. |
| **Session Pinning UI** | Production-Ready | `app.js:500-600` | Holds edits during agent sessions and releases them cleanly. |
| **Micro-Haptic Audio** | Production-Ready | `app.js:51-102` | Web Audio API sound feedback on sync, error, and connection events. |

---

## 3. How It Works Under the Hood

### Architecture & Runtime Flow

```
+-------------------------------------------------------------------+
|                        Browser Frontend                           |
|  (HTML5 / CSS / Vanilla JS / Web Audio API / EventSource Client)  |
+-------------------------------------------------------------------+
             |                                        ^
      HTTP REST Requests                     Server-Sent Events (SSE)
     (Bearer Token Auth)                     (/api/events channel)
             |                                        |
             v                                        |
+-------------------------------------------------------------------+
|                   Axum HTTP DashboardServer                       |
|           (crates/ferry-daemon/src/ui/server.rs)                  |
+-------------------------------------------------------------------+
             |
       AutoBackend (crates/ferry-ipc/src/backend.rs)
             |
             +---> [If Daemon Running] ---> IPC Socket ---> Daemon Supervisor
             |
             +---> [If Standalone]     ---> In-Process Direct Engine & Store
```

### Key API Endpoints
- `GET /` & `/app.js`: Serves the embedded single-page application.
- `GET /api/status`: Returns current sync state, peer status, and folder statistics.
- `GET /api/events`: SSE stream broadcasting `UiEvent::StateChanged` and `UiEvent::ConflictRecorded`.
- `POST /api/share`: Initiates pairing and returns a 6-character Base32 code with QR payload.
- `POST /api/pair/join`: Adopts a folder and connects to the offering peer using a pairing code.
- `POST /api/pin/start`, `/api/pin/stop`, `/api/pin/release`: Controls agent session hold state.
- `GET /api/conflicts`: Returns the structured quarantine conflict ledger.

### Authentication Model
When `ferry ui --web` starts, it generates a 32-character hexadecimal token. The token is passed via URL query parameter (`?token=<hex>`). The client stores it in `sessionStorage` and injects it into the `Authorization: Bearer <token>` header for all subsequent API calls.

---

## 4. Step-by-Step Testing Guide: Two Machines

Follow these steps to test Ferry between Machine A (Offering Device) and Machine B (Joining Device).

### Prerequisites
Both machines must have the repository built:
```bash
cargo build --release
# Binary will be at target/release/ferry
```

---

### Step 1: Initialize and Share on Machine A

1. Navigate to the project directory on Machine A:
   ```bash
   cd ~/my-test-project
   ```

2. Initialize Ferry if the directory is not already tracked:
   ```bash
   target/release/ferry init
   ```

3. Launch the Web UI:
   ```bash
   target/release/ferry ui --web
   ```
   *Tip: To access the Web UI from another device over your local network, bind to all interfaces:*
   ```bash
   target/release/ferry ui --web --host 0.0.0.0 --port 8080
   ```

4. The browser will open automatically at `http://127.0.0.1:<PORT>/?token=<TOKEN>`.

5. In the Web UI:
   - Click the **Share** button in the header or action bar.
   - The Share Modal will display a **6-character pairing code** (e.g. `K9X2M4`) and a QR code.
   - Keep this modal open. The code is active for 10 minutes.

---

### Step 2: Join on Machine B

1. On Machine B, create an empty destination directory:
   ```bash
   mkdir -p ~/my-test-project
   cd ~/my-test-project
   ```

2. Launch the Web UI on Machine B:
   ```bash
   target/release/ferry ui --web
   ```

3. In the Web UI on Machine B:
   - Click the **Pair / Join** button.
   - Enter the 6-character code displayed on Machine A (e.g. `K9X2M4`).
   - Click **Join Folder**.

---

### Step 3: Observe Real-Time Convergence

1. **Beacon Status:** Both dashboards will transition from `Connecting` &rarr; `Syncing` (pulsing blue) &rarr; `Synced` (steady green).
2. **Test File Sync:**
   - Create a file on Machine A:
     ```bash
     echo "Hello from Machine A" > ~/my-test-project/test.txt
     ```
   - Watch Machine B. Within sub-seconds, `test.txt` will appear on disk, and the Web UI counter will update live via SSE.
3. **Test Session Pinning (Agent Hold Mode):**
   - On Machine A, click **Hold Edits** in the Web UI.
   - Modify a file on Machine B.
   - Notice that Machine A holds the edit in `.ferry/held/` without overwriting active files.
   - Click **Release & Merge** on Machine A to apply the held changes.
4. **Test Conflict Quarantine:**
   - Modify `test.txt` concurrently on both machines.
   - The winner is retained as `test.txt`.
   - The loser is quarantined as `test.txt.ferry-conflict.<device>-<ts>`.
   - The Conflicts banner in the Web UI lights up with a structured diff view.

---

## 5. Where Things Live

- CLI Command Entry: `crates/ferry-cli/src/commands/ui.rs`
- Axum Web Server & API Handlers: `crates/ferry-daemon/src/ui/server.rs`
- Backend Bridge & AutoBackend: `crates/ferry-daemon/src/ui/backend.rs`
- HTML Shell: `crates/ferry-daemon/assets/index.html`
- Stylesheet: `crates/ferry-daemon/assets/style.css`
- JavaScript Application: `crates/ferry-daemon/assets/app.js`

---

## 6. Gotchas & Troubleshooting

1. **Browser Blocks Autoplay Audio:** Modern browsers require one user click before the Web Audio API can play micro-haptic sound effects.
2. **Token Lost After Clearing Storage:** If you open `http://localhost:<PORT>` without the `?token=` parameter, the UI displays a Token Required modal. Copy the token from your terminal output and paste it into the input.
3. **Inactivity Auto-Shutdown:** The web server automatically shuts down after 10 minutes of complete inactivity to conserve battery and CPU. Refreshing or interacting resets the timer.
4. **LAN Discovery vs Relay:** If both machines share a local WiFi network, QUIC connects directly via mDNS. If they are on separate networks, connections route through the blind relay fallback seamlessly.
