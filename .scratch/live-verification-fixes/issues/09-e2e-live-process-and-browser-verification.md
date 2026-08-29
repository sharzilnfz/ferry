# 09: End-to-End Live Process & Playwright Browser Verification

Status: done
Depends on: 01-core-sync-three-way-reconciliation.md, 02-daemon-ipc-server-pin-liveness.md, 03-cli-pin-hours-and-ignore-folder.md, 04-api-status-peer-agreement-alignment.md, 05-async-pairing-workflow.md, 06-resilient-sse-streaming-polling-fallback.md, 07-minimalist-web-ui-overhaul.md, 08-honest-token-auth-session-storage.md
Blocks: None

**What to build:**
Comprehensive end-to-end integration and verification suites across the entire fixed surface. Execute dual-node live process tests over loopback TCP and IPC sockets verifying that unpinned concurrent modifications quarantine cleanly without data loss, daemon pin ownership maintains protection after CLI exit, CLI flags operate as documented, and automated Playwright browser tests confirm the new minimalist UI renders correctly, switches themes, pairs without blocking, and remains fully functional under desktop and mobile viewports.

**Blocked by:**
- 01: Core Sync Three-Way Reconciliation & Unpinned Conflict Quarantine
- 02: Daemon IPC Server Binding & Long-Lived Pin Ownership
- 03: CLI Flag & Argument Parity (`--hours` & `ignore` folder targeting)
- 04: API Status Alignment & Synchronized Peer Agreement Badging
- 05: Asynchronous Non-Blocking Folder Pairing & Status Handshake
- 06: Resilient SSE Event Streaming with Silent Polling Fallback
- 07: Minimalist, Zero-Jargon Web Interface Overhaul (Captive Portal Style)
- 08: Honest Token Authentication & Session Storage Flow

### Acceptance Criteria

- [x] Dual-node live synchronization test demonstrates unpinned concurrent file modifications result in one winner and one quarantined conflict file (`*.ferry-conflict.*`), with an entry appended to `.ferry/conflicts.jsonl`.
- [x] Live daemon test asserts `ferry pin start --hours 8` keeps the pin session active across multiple subsequent CLI commands and verifies holds against incoming peer writes.
- [x] CLI regression tests assert `ferry ignore --list <folder>` successfully reads rule layers from the specified directory path.
- [x] Automated Playwright browser tests verify:
  - Hero status banner displays correct text (`SYNCED`, `HOLDING`, `CONFLICTS`) and color dot.
  - Connected devices display green agreed badges when synchronized.
  - Theme toggle switches between dark and light modes cleanly and persists across page reloads.
  - Asynchronous share flow displays short code immediately without blocking the browser interface.
  - Responsive design renders cleanly on mobile viewport (390 × 844 px) without overflow.
  - Token authentication persists across page navigation via session storage.
- [x] Full workspace test suites (`cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`) pass cleanly.
