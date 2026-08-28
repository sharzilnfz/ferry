# Dual-Device End-to-End Test Suite Report

**Date**: 2026-08-27  
**Devices**: Apple MacBook Air (`100.91.38.24`) $\longleftrightarrow$ Arch Linux Laptop (`sharzil@sharzilx` / `100.122.159.26`)  
**Multiplexer Environment**: Herdr (`HERDR_ENV=1`)  
**Network Transport**: Tailscale Mesh (WireGuard)  
**Binary Version**: `ferry 0.1.0` (Release build on both nodes)  
**Result**: **6/6 Stages Passed (100% Assertion Success)**

---

## Executive Summary

A zero-touch, dual-device end-to-end test suite was executed across the local **MacBook Air** and the remote **Arch Linux laptop** (`sharzil@sharzilx`). All 6 stages passed, confirming cryptographic 3-way pairing, live background daemon execution via Herdr terminal multiplexing, sub-5-second bidirectional convergence across heterogeneous binary and text payloads (including a 1 MB payload), active session pinning hold broadcasting, and authenticated Web UI / REST / SSE validation.

---

## Latency & Scoreboard Summary

| Stage | Test Name | Target / Subsystem | Status | Latency / Convergence |
| :--- | :--- | :--- | :---: | :---: |
| **Stage 1** | **Environment & Connectivity Check** | `100.91.38.24` $\longleftrightarrow$ `100.122.159.26` | **`PASS`** | RTT: **5.49 ms** |
| **Stage 2** | **Isolated Workspace & Pairing Ritual** | 3-Way Payload Exchange (`FRPO`/`FRPR`/`FRGR`) | **`PASS`** | **1,506.1 ms** |
| **Stage 3** | **Live P2P Sync Daemons (Herdr Panes)** | TCP Listener (`:44001`) & Dial Connector | **`PASS`** | **3,648.4 ms** |
| **Stage 4.1** | **Mac $\rightarrow$ Arch Sync (10 Files + 1MB Bin)** | Continuous scan & chunk exchange | **`PASS`** | **3,579.8 ms** *(limit < 5.0s)* |
| **Stage 4.2** | **Arch $\rightarrow$ Mac Sync (Tree + Append Log)** | Nested dirs + 100 log lines append | **`PASS`** | **932.5 ms** *(limit < 5.0s)* |
| **Stage 5.1** | **Session Pinning Telemetry** | `ferry pin start --paths 'logs/app.log'` | **`PASS`** | Pinned (PID: 171051) |
| **Stage 5.2** | **Web Dashboard, REST & SSE Streams** | `GET /`, `/style.css`, `/app.js`, `/api/*` | **`PASS`** | HTTP 200 / SSE Active |
| **Stage 6** | **Graceful Teardown & Pane Cleanup** | Herdr Panes (`wS:pF`, `wS:pG`, `wS:pH`) | **`PASS`** | Clean SIGINT Shutdown |

---

## Detailed Stage Analysis

### Stage 1: Environment & Network Reachability
- **Mac Binary**: `/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry` (`ferry 0.1.0`, 6,483,856 bytes)
- **Arch Binary**: `/home/sharzil/.cargo/bin/ferry` (`ferry 0.1.0`, 7,597,312 bytes)
- **Tailscale Ping Statistics**:
  - Mac $\rightarrow$ Arch: `avg = 5.494 ms` (0.0% packet loss)
  - Arch $\rightarrow$ Mac: `avg = 33.036 ms` (0.0% packet loss)

### Stage 2: Isolated Workspace & Cryptographic Pairing
- **Folder Master Key (FMK) Wrap Agreement**:
  - Assigned Folder ID: `3259cdad6d8ed91c433b7bb2ee90a7b9`
  - Mac Device ID: `3d5b5d41d1e24244e792830c196c3fd42089f38650862eee6fb7743449bca447`
  - Arch Device ID: `99e0942a138f0e53342dbc3fdb760e6dd9137d96352bec65eb3ea45b2af8b043`
- **3-Way Out-of-Band Ritual**:
  1. `ferry pair` on Mac generated 93-byte `pair-offer.ferry-pair`.
  2. Arch executed `ferry pair --accept` generating `pair-response.ferry-pair`.
  3. Mac verified response transcript MAC, added Arch wrap to `CONFIG_HEAD`, and sealed `pair-grant.ferry-grant`.
  4. Arch decrypted FMK and adopted local store. Both `.ferry/config` files contain 2 device wrap entries.

### Stage 3: Live P2P Sync Daemons (Herdr Multiplexing)
- **Mac Listener Daemon**: Spawned in Herdr pane `wS:pF` listening on `0.0.0.0:44001`.
- **Arch Connector Daemon**: Spawned in Herdr pane `wS:pG` dialing peer `100.91.38.24:44001`.
- TCP handshake established and live scanning active at 200ms tick interval.

---

## File Integrity Checksums (SHA-256)

### Mac $\rightarrow$ Arch Synchronization (10 Files)
*Total Convergence Time: **3,579.87 ms***

| Filename | Size | Mac Origin SHA-256 | Arch Synced SHA-256 | Byte-Exact Match |
| :--- | :--- | :--- | :--- | :---: |
| `source.rs` | 82 B | `de3c2d4879b2d6a8f538e3fa740c9ce9a38448ea23efb7cc136214937752c8fb` | `de3c2d4879b2d6a8f538e3fa740c9ce9a38448ea23efb7cc136214937752c8fb` | **MATCH** |
| `README.md` | 70 B | `aafa106582ce3718b2fe09cfad949c70f334d8638d6dec78d0922571c52454e8` | `aafa106582ce3718b2fe09cfad949c70f334d8638d6dec78d0922571c52454e8` | **MATCH** |
| `config.json` | 85 B | `2d7496c8421fbb4179996b96261132822c0eae86afdc115767e5c0034cfb1910` | `2d7496c8421fbb4179996b96261132822c0eae86afdc115767e5c0034cfb1910` | **MATCH** |
| `settings.yaml` | 76 B | `814c43cf2e786476310392a0113c7b82670982ca742255b98d057e2fe0fa4cea` | `814c43cf2e786476310392a0113c7b82670982ca742255b98d057e2fe0fa4cea` | **MATCH** |
| `docker-compose.yml` | 78 B | `9800893b3904329b7d4664303b424f7f77a5c7a249a76be760c623697f45fd5a` | `9800893b3904329b7d4664303b424f7f77a5c7a249a76be760c623697f45fd5a` | **MATCH** |
| `binary_1k.dat` | 1.00 KB | `35882a3448b7518646366c0def621ffc629a76b0b63dc87eb3c1e94876cc2b4a` | `35882a3448b7518646366c0def621ffc629a76b0b63dc87eb3c1e94876cc2b4a` | **MATCH** |
| `binary_10k.dat` | 10.0 KB | `e41cc7bb7ad99a2ab489480b9aa69721d5525c4a17ca96dbc32c6cceafd0d7f8` | `e41cc7bb7ad99a2ab489480b9aa69721d5525c4a17ca96dbc32c6cceafd0d7f8` | **MATCH** |
| `binary_100k.dat` | 100 KB | `309fce07d16d48624505a537ce0b4286971cc8172717c78db8913c2f32c35afb` | `309fce07d16d48624505a537ce0b4286971cc8172717c78db8913c2f32c35afb` | **MATCH** |
| `binary_500k.dat` | 500 KB | `1f59218b55d54c2dbf715e9d3091448953d026c45efe315bd5ac80c328c1504a` | `1f59218b55d54c2dbf715e9d3091448953d026c45efe315bd5ac80c328c1504a` | **MATCH** |
| `payload_1mb.bin` | 1.00 MB | `8a0b889ab3802beb68dcef4d6ad1747292b56e6f04e00800a00364b756e16f81` | `8a0b889ab3802beb68dcef4d6ad1747292b56e6f04e00800a00364b756e16f81` | **MATCH** |

### Arch $\rightarrow$ Mac Synchronization (Nested Trees & Appends)
*Total Convergence Time: **932.52 ms***

| Filename | Arch Origin SHA-256 | Mac Synced SHA-256 | Match |
| :--- | :--- | :--- | :---: |
| `nested/deep/level3/service/app.toml` | `56d8b549c89ec1cffa5591cd53183e53907a1d6a6ea66a1ee355416813e75feb` | `56d8b549c89ec1cffa5591cd53183e53907a1d6a6ea66a1ee355416813e75feb` | **MATCH** |
| `logs/app.log` *(100 lines)* | `86c55df9683fb1c7f8d2415f3f54ea94127e4322c3adcdb0c667cd71f98a3af5` | `86c55df9683fb1c7f8d2415f3f54ea94127e4322c3adcdb0c667cd71f98a3af5` | **MATCH** |

---

## Session Pinning & Web Dashboard Telemetry

1. **Session Pinning**:
   - `ferry pin start --paths 'logs/app.log'` executed successfully on Arch Linux (PID: 171051).
   - Peer hold was broadcast across the active daemon TCP link.
   - Clean release via `ferry pin stop`.

2. **Web Dashboard (`ferry ui --port 8098`)**:
   - Bound to `http://127.0.0.1:8098/?token=633c8504998688c3e344a442ba261aec` in Herdr pane `wS:pH`.
   - `GET /` $\rightarrow$ **HTTP 200** (served 11.8 KB HTML SPA).
   - `GET /style.css` & `GET /app.js` $\rightarrow$ **HTTP 200**.
   - `GET /api/status?token=...` $\rightarrow$ **HTTP 200** (returned accurate manifest state, peer array, and 16 synchronized files).
   - `GET /api/conflicts?token=...` $\rightarrow$ **HTTP 200** (0 conflicts).
   - `GET /api/events?token=...` $\rightarrow$ **HTTP 200 SSE** (stream opened and pushed initial `event: state` frame).

---

## Teardown & Artifacts

- Process signals sent: `SIGINT` (Ctrl+C) dispatched to Herdr worker panes (`wS:pF`, `wS:pG`, `wS:pH`).
- Panes safely closed, terminating all daemons without dangling sockets.
- Machine-readable raw JSON data: `.scratch/dual_device_e2e_results.json`
- Test runner script: `scripts/dual_device_e2e_test.py`
