# Spec: Live Verification Gap Remediation & Minimalist Web UI

Status: ready-for-agent

## Problem Statement

Developers and autonomous coding agents relying on Ferry encounter critical functional gaps, CLI discrepancies, and UX friction that compromise data safety and usability:

1. **Silent Data Loss Risk**: When two unpinned devices edit the same file concurrently, Ferry's core sync engine falls back to a simplistic two-way overwrite instead of three-way reconciliation. The remote modification wipes out the local copy without generating a conflict quarantine file or logging an entry to the conflict ledger, directly violating the core product promise of zero silent data loss.
2. **Immediate Pin Invalidation**: Running `ferry pin start` via the command line records the short-lived CLI process ID because the background daemon fails to bind its IPC server. As soon as the CLI command terminates, the recorded PID dies, causing the pin to be flagged as stale immediately and allowing incoming remote changes to overwrite the working directory unprotected.
3. **CLI Contract Discrepancies**: The CLI parser lacks the documented `--hours` flag on `ferry pin start`, preventing users from setting temporary protection windows as documented. Furthermore, `ferry ignore` does not accept a directory argument, causing commands targeting external folders to fail with confusing error messages.
4. **Misleading Web Dashboard State**: In the web UI, connected devices that are completely synchronized are permanently marked with an amber "not agreed" badge because the dashboard compares a root directory tree identifier against a signed manifest blob identifier.
5. **Thread-Blocking Pairing Ritual**: Triggering folder sharing from the web UI synchronously blocks the HTTP server thread for up to 120 seconds waiting for an out-of-band response, freezing the browser interaction and preventing the user from viewing the generated short code or payload file.
6. **Console Errors on Event Streaming**: The event stream endpoint returns a 501 Not Implemented response, causing uncaught JavaScript errors in the browser console.
7. **Contradictory Authentication Messaging**: The web dashboard strictly requires a 32-character hexadecimal token for access, rejecting unauthenticated users with a 403 Forbidden error while displaying a footer that paradoxically claims the UI has no authentication by design.
8. **Visual Complexity and Jargon Overload**: The web UI exposes raw hexadecimal hashes, Merkle root pointers, cryptographic terminology, and dense key-value tables. This creates high cognitive load for developers who simply want a clean, minimalist, high-efficiency interface indicating whether their workstation and remote machines are in sync.

---

## Solution

A comprehensive remediation across the core synchronization engine, CLI interfaces, background daemon, and embedded web dashboard:

1. **Integrate Three-Way Reconciliation into Core Sync**:
   Promote the three-way reconciler into the core exchange pipeline. When unpinned concurrent changes occur, Ferry compares the local tree, remote tree, and last-agreed base manifest, deterministically preserving the newer modification in place while quarantining the losing file alongside it and logging a structured entry in the conflict ledger.
2. **Daemon IPC Service & Long-Lived Pin Ownership**:
   Ensure the background daemon automatically binds and listens on a dedicated local domain socket or named pipe. CLI commands communicate with the active daemon, transferring pin session ownership to the long-lived daemon process so protection windows remain valid across CLI invocations.
3. **Harmonize CLI Flags and Argument Handling**:
   Add `--hours` support to the session pinning command and allow selective ignore commands to accept an explicit folder path.
4. **Align Status Identifiers for Peer Agreement**:
   Unify the manifest comparison keys returned by status queries so synchronized devices accurately display a green agreed status.
5. **Asynchronous Non-Blocking Pairing in Web UI**:
   Split the web pairing ritual into an immediate initiation phase (returning the short code, payload path, and pending status instantly) and an asynchronous completion check, eliminating thread freezes.
6. **Graceful Real-Time Updates & Error Handling**:
   Provide clean real-time status streaming over server-sent events with seamless, silent degradation to background polling without console exceptions.
7. **Honest, Frictionless Token Authentication**:
   Align the security posture by removing contradictory footer statements and implementing persistent session token storage so users accessing the UI with an authorized link stay authenticated across reloads and tab navigations.
8. **Minimalist, Zero-Jargon Web Interface**:
   Overhaul the embedded web UI inspired by high-efficiency utility dashboards:
   - **Hero Status Display**: Large, unmistakable status banner (`SYNCED`, `HOLDING`, `CONFLICTS DETECTED`, `OFFLINE`) accompanied by a distinct status dot.
   - **Human-Centric Language**: Plain English summaries ("All files up to date with 2 devices", "Protection active — external changes held") with zero mentions of raw hashes, encryption algorithms, or internal leases.
   - **Two-Column Desktop / Responsive Single-Column Layout**:
     - **Left Pane (Activity Stream)**: A clean monospaced console tracking recent sync milestones, holds, and peer events with timestamps.
     - **Right Pane (Control Cards)**: Modular, low-profile cards for Connected Devices, Work Protection (Session Pinning), and Conflict Resolution.
   - **Theme System**: Dedicated dark and light modes with crisp contrast and a single-click header toggle.

---

## User Stories

### Core Synchronization & Data Safety
1. As a developer editing code on my laptop while an agent modifies the same file on my desktop, I want Ferry to detect concurrent unpinned modifications and quarantine the losing edit, so that no work is ever silently overwritten.
2. As a developer reviewing a synchronization conflict, I want the losing file preserved with its timestamp and originating device tag, so that I can easily inspect and recover changes.
3. As an automated script inspecting synchronization health, I want conflicts recorded in a structured JSONL ledger, so that tooling can parse and alert on pending divergences.
4. As a developer synchronizing large binary files and source trees, I want content-defined chunking and deduplication preserved alongside three-way conflict detection, so that performance remains high.

### CLI Usability & Session Pinning
5. As an agent or developer initiating focused work, I want to run `ferry pin start --hours 8`, so that remote changes to my working directory are held back for the duration of my work session.
6. As a developer checking pin status with `ferry pin status`, I want the pin to remain active after the initiating CLI command exits, so that the background daemon continues protecting my files.
7. As a developer managing ignore rules across multiple repositories, I want to run `ferry ignore --list /path/to/project`, so that I can inspect active ignore rules without changing my working directory.
8. As a developer with active held changes, I want `ferry pin release` to reconcile all held modifications through the three-way reconciler, so that winners apply cleanly and losers quarantine without data loss.

### Web Dashboard: Visual Clarity & Zero Jargon
9. As a developer glancing at the web dashboard, I want to see a massive, high-contrast status header (`SYNCED`, `HOLDING`, `CONFLICTS`), so that I immediately know the health of my workspace without reading tables.
10. As a developer navigating the interface, I want status explanations written in plain English without cryptographic jargon (such as Merkle roots, ED25519 keys, or CDC pack IDs), so that I understand my project state without needing domain expertise.
11. As a developer working late at night or in bright daylight, I want a theme toggle to switch between dark and light modes, so that the dashboard is comfortable to view in any environment.
12. As a mobile developer monitoring a build from a phone or tablet, I want the dashboard to collapse into a single-column layout, so that all controls and status feeds are fully accessible on smaller screens.
13. As a developer monitoring sync events, I want an activity console showing timestamped actions and status transitions, so that I have a continuous audit log of what Ferry is doing.

### Web Dashboard: Devices, Pairing & Protection
14. As a developer pairing a second computer, I want the "Share" action to return a short code and payload instructions immediately, so that the web page remains responsive and interactive.
15. As a developer with paired machines, I want connected devices to display an accurate green status badge when synchronized, so that I can trust that both machines hold identical files.
16. As a developer starting an uninterrupted editing session from the browser, I want to click a single "Protect Work" button, so that incoming remote changes are safely held in the background.
17. As a developer finishing a protected session from the browser, I want to click "Release", so that all held changes are evaluated and integrated without clobbering my local files.
18. As a developer with unresolved conflicts, I want a dedicated conflicts panel showing the affected files and quarantine locations, so that I can inspect and resolve them quickly.

### Real-Time Updates & Security
19. As a browser user, I want the dashboard to receive real-time updates over server-sent events without throwing JavaScript console errors, so that the interface reflects live sync state seamlessly.
20. As a browser user on a restricted network where event streaming drops, I want the dashboard to silently degrade to background polling, so that status remains fresh without disruptive error banners.
21. As a security-conscious developer, I want the web dashboard to require a local session token to prevent unauthorized loopback queries from third-party browser tabs, so that my files remain secure.
22. As a developer opening the dashboard via `ferry ui`, I want the authentication token stored in browser session storage, so that page refreshes do not lock me out.
23. As a developer reading the dashboard footer, I want the security text to accurately state the protection model (`Localhost only · Protected by session token`), so that there is no confusion regarding authentication.

---

## Implementation Decisions

### 1. Core Synchronization Reconciler Integration
- **Promote Reconciler to Core Dependency**: Move the three-way reconciliation crate from test-only dependencies into the core synchronization library dependencies.
- **Three-Way Exchange Evaluation**: Replace direct two-way diff application in the pull phase of the exchange pipeline with a three-way reconciliation step comparing the local tree, remote tree, and the last-agreed base manifest.
- **Deterministic Conflict Quarantine**: When concurrent modifications are identified on an unpinned file, the reconciler determines the winning revision based on modification timestamps (with device identifier tiebreaking), keeps the winner live in the tree, writes the losing revision to a conflict quarantine path (`<path>.ferry-conflict.<device>-<timestamp>`), and appends a record to the conflict ledger.

### 2. Background Daemon IPC Service
- **Automatic IPC Socket Binding**: When the background daemon starts watching a folder, it automatically binds and serves an IPC endpoint (Unix domain socket on macOS/Linux, named pipe on Windows) located within the folder's state directory.
- **Daemon-Owned Pin Sessions**: When `ferry pin start` executes, it transmits a pin command over IPC to the running daemon. The daemon registers the pin and associates it with the daemon's own long-lived process identifier. If no daemon is running, the CLI issues a clear notification instructing the user to launch `ferry daemon` for background protection.
- **Duration and Expiration Enforcement**: Update the session pin data structure to support an expiration timestamp calculated from the provided duration. The daemon's internal timer evaluates pin expiration during each scan cycle, marking expired pins as released.

### 3. CLI Interface Enhancements
- **Pin Duration Flag**: Add an optional `--hours` argument (defaulting to 8 hours when omitted) to the CLI pin command parser. Pass this duration through the IPC command to the daemon.
- **Target Folder in Ignore Command**: Update the CLI ignore command parser to accept an optional target directory argument. Ensure path normalization and folder validation operate against the specified target rather than assuming the current working directory.

### 4. API Document Alignment & Agreement State
- **Manifest ID Normalization**: Ensure status queries return the signed manifest blob identifier consistently across both offline inspections and live daemon endpoints.
- **Precise Peer Agreement Matching**: In the web UI and status documents, evaluate peer agreement by comparing the current folder's manifest blob identifier with the peer's recorded last-agreed manifest blob identifier. When both match, the peer is marked as synchronized.

### 5. Non-Blocking Pairing Workflow
- **Two-Stage Web Pairing**: Split the pairing operation in the web UI backend:
  - An initiation endpoint (`POST /api/share`) generates the pairing payload and short code immediately, returning status `pending` within milliseconds.
  - A completion endpoint or polling query checks the file system for the corresponding accept payload without blocking HTTP handler threads.
- **Pairing Dialog in UI**: The web frontend displays the generated short code and payload file location with a live waiting spinner, allowing the user to copy the code while waiting for the remote device to accept.

### 6. Minimalist Web UI Overhaul
- **Aesthetic Direction**: Strict adherence to the clean, typography-driven, zero-jargon visual style established in the reference review screenshots:
  - Monospaced badges and system indicators.
  - Ultra-bold grotesque typography for primary status words (`SYNCED`, `HOLDING`, `CONFLICTS`, `OFFLINE`).
  - Restrained border styles (`1px solid #1a1a1a` in dark mode, `#e5e5e5` in light mode).
  - Prominent status indicator dot (green for synced, amber for holding/warning, red for conflict).
- **Layout Architecture**:
  - **Header**: Minimal top bar with application identity, version indicator, theme toggle button (sun/moon icon), and session connectivity dot.
  - **Hero Banner**: Full-width status section displaying current synchronization state and a plain-English sub-heading describing current peer convergence.
  - **Quick Action Bar**: High-prominence primary actions ("Sync Now", "Protect Work", "Pair Device").
  - **Two-Column Split Body**:
    - **Activity Feed (Left)**: Darkened monospaced terminal card displaying recent events with timestamps and colored status dashes.
    - **Control Cards (Right)**: Stacked cards for "Devices" (device nickname, last synced time, pairing trigger) and "Work Protection" (status, hold scope, release button).
  - **Footer**: Single centered line with documentation link, version number, and honest security notice.
- **Theme Support**: Pure CSS variable-based theming supporting dark mode (default) and light mode, persisted across page loads via local storage.
- **Zero-Jargon Copy Policy**:
  - Replace "Merkle root" and "Blob ID" with "Files" or "Snapshot".
  - Replace "Session Pinning" with "Work Protection".
  - Replace "Peer / Lineage" with "Connected Devices".
  - Replace "CDC / Packs" with "Storage".

### 7. Resilient Event Streaming & Error Handling
- **SSE Stream Implementation**: Provide a lightweight event stream handler in the web server that subscribes to the daemon's internal state broadcast receiver and emits `state` events.
- **Silent Degradation**: If the SSE connection fails or is unsupported, the client JavaScript silently switches to a 2-second background polling cycle without logging uncaught errors to the console.

### 8. Session Token Authentication & Storage
- **Browser Token Persistence**: When the dashboard is accessed with a `?token=<hex>` query parameter, the client script caches the token in `sessionStorage` and attaches it via the `Authorization: Bearer <token>` header on all subsequent API requests.
- **Token Input Modal**: If the dashboard is accessed without a token, rather than rendering a raw 403 error page, the UI presents a minimal, friendly token entry prompt.
- **Footer Text Alignment**: Update the dashboard footer to accurately declare: `Localhost only · Protected by session token`.

---

## Testing Decisions

### What Makes a Good Test
Tests for this feature must evaluate end-to-end process behavior, data integrity, and protocol contracts from the outside. They must avoid asserting internal lock states or private helper logic. Specifically:
- Two real daemon processes running against isolated directories must achieve convergence.
- Concurrent unpinned file writes must produce verified conflict files and ledger entries.
- CLI commands executed against a live daemon must succeed and alter daemon state.
- Web API endpoints and browser UI flows must be validated via automated HTTP requests and browser-level tests.

### Target Test Suites
1. **Core Reconciliation Suite**:
   - Verify unpinned concurrent file updates between two exchanging nodes. Assert that the newer file remains active and the losing file is quarantined with the loser's device short ID in its filename.
   - Verify that simultaneous deletions versus modifications resurrect the modified file rather than allowing it to disappear.
2. **IPC & Pinning Integration Suite**:
   - Launch a background daemon process and execute `ferry pin start --hours 4` from a separate CLI process. Assert that `ferry pin status` immediately reports active holding and does not degrade to stale upon CLI process exit.
   - Test `ferry ignore --list <path>` with an explicit path argument, confirming rules are read from the target directory.
3. **Web Server & UI Integration Suite**:
   - Query `/api/status` and verify that peer agreement reflects manifest convergence accurately.
   - Test `POST /api/share` and verify non-blocking immediate response containing the short code.
   - Validate SSE `/api/events` connectivity and test fallback to polling upon stream termination.
   - Execute Playwright browser tests asserting:
     - Hero status banner renders with correct text and color.
     - Dark/light mode theme toggle updates DOM attributes and persists in local storage.
     - Responsive layout collapses gracefully on mobile viewport (390px width).
     - Token authentication passes from URL query to session storage without rejection.

### Prior Art
- `crates/ferry-sync/tests/`: Integration tests for transport and one-shot sync.
- `crates/ferry-cli/tests/ipc_cli_integration.rs`: IPC client/server query tests.
- `crates/ferry-cli/tests/ui_server_tests.rs`: Axum web server integration tests.
- `scripts/quickstart-e2e.sh`: Two-device process convergence verification script.

---

## Out of Scope

- Implementing automatic line-by-line file merging (violates ADR-0004; all conflicts must quarantine).
- Hosted cloud relays or public internet discovery infrastructure (Ferry remains local-first and peer-to-peer).
- Introducing Node.js or JavaScript build tools (the web interface remains self-contained vanilla HTML/CSS/JS embedded directly into the Rust binary).
- Multi-party consensus or quorum replication across more than two concurrently writing devices.

---

## Further Notes

- All changes must adhere to project coding guidelines: Rust code formatted via `cargo fmt`, passing `cargo clippy --workspace --all-targets -- -D warnings`, and strict preservation of existing data files and ADR contracts.
- The web assets (`index.html`, `style.css`, `app.js`) remain zero-dependency static assets compiled into the `ferry-daemon` binary via `include_bytes!`.
