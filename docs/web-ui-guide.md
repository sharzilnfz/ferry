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
Build the binary on both machines:
```bash
cargo build -p ferry-cli              # Fast debug build -> ./target/debug/ferry
# Or: cargo build --release -p ferry-cli -> ./target/release/ferry
```

---

### Step 1: Initialize and Pair Devices

Initialize and pair your demo folder using the offer-file flow:
```bash
# On Machine A:
mkdir -p /tmp/ferry-sync-demo && cd /tmp/ferry-sync-demo
./target/debug/ferry init
./target/debug/ferry pair

# On Machine B:
mkdir -p /tmp/ferry-sync-demo && cd /tmp/ferry-sync-demo
scp mac:/tmp/ferry-sync-demo/.ferry/pair-offer.ferry-pair /tmp/pair-offer.ferry-pair
./target/debug/ferry pair --accept /tmp/pair-offer.ferry-pair /tmp/ferry-sync-demo

# Finish roundtrip:
scp arch:/tmp/pair-response.ferry-pair /tmp/ferry-sync-demo/.ferry/pair-response.ferry-pair
scp /tmp/ferry-sync-demo/.ferry/pair-grant.ferry-grant arch:/tmp/pair-grant.ferry-grant
```

---

### Step 2: Start Daemon and Launch Web UI

1. **On Machine A (Start Sync Server + Web UI):**
   ```bash
   ./target/debug/ferry daemon --listen 0.0.0.0:44001 /tmp/ferry-sync-demo &
   ./target/debug/ferry ui --web --port 8080 /tmp/ferry-sync-demo
   ```
   *(The terminal prints the URL with the one-time token: `http://127.0.0.1:8080/?token=<hex>`)*

2. **On Machine B (Connect Peer Daemon):**
   ```bash
   ./target/debug/ferry daemon --peer-url <MACHINE_A_IP>:44001 --interval-secs 1 /tmp/ferry-sync-demo
   ```

---

### Step 3: Observe Real-Time Dashboard Updates

1. **Dashboard Overview:**
   - Open `http://127.0.0.1:8080/?token=<token>` in your browser.
   - Storage statistics, connected fleet peers, and folder manifest hashes render in real-time.

2. **Test File Sync:**
   - Create a file in `/tmp/ferry-sync-demo/test.txt` on either machine.
   - The Web UI updates the file count and manifest live via Server-Sent Events (SSE) without a page refresh.

3. **Verify Token Security:**
   ```bash
   # Blocked without token (403 Forbidden):
   curl -i "http://127.0.0.1:8080/api/status"

   # Allowed with token (200 OK):
   curl -i "http://127.0.0.1:8080/api/status?token=<token>"
   ```
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
