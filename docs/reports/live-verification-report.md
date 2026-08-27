# Live Verification & Remediation Report

**Date**: 2026-08-27  
**Branch**: `arch-hardening`  
**Workspace Status**: All tests passing (`cargo test --workspace`), clippy clean (`cargo clippy --workspace --all-targets -- -D warnings`), 0 errors, 0 warnings.

---

## Executive Summary

A full end-to-end remediation and verification cycle was performed across Ferry's core sync engine, background daemon, IPC subsystem, CLI interfaces, and embedded web dashboard. All nine blocking live verification issues identified in `.scratch/live-verification-fixes/` have been resolved, integrated, and verified with live dual-node process tests and Playwright browser verification.

---

## Remediated Issues Summary

| Issue ID | Title | Component | Status | Key Verifications |
| :--- | :--- | :--- | :--- | :--- |
| **01** | Core Sync Three-Way Reconciliation | `ferry-sync`, `ferry-sync-engine` | **DONE** | Concurrent unpinned writes deterministically quarantine losing file (`*.ferry-conflict.*`) and append structured record to `.ferry/conflicts.jsonl`. |
| **02** | Daemon IPC Binding & Pin Liveness | `ferry-daemon`, `ferry-ipc` | **DONE** | Daemon automatically binds local domain socket. Pins are owned by long-lived daemon PID, surviving CLI process exit. |
| **03** | CLI `--hours` & `ignore` Folder Targeting | `ferry-cli`, `ferry-pin` | **DONE** | Added `--hours <N>` (default 8) to `ferry pin start`. Added optional `[FOLDER]` to `ferry ignore` across list, preset, and pattern rules. Expiration timestamps tracked and enforced. |
| **04** | API Status Agreement Alignment | `ferry-daemon`, `ferry-store` | **DONE** | Unify manifest IDs returned in status queries against peer agreement ledger records. Synchronized devices accurately show green agreed badges. |
| **05** | Asynchronous Pairing Workflow | `ferry-daemon`, `ferry-folder` | **DONE** | Web pairing returns short code and offer payload path immediately (< 50ms) with non-blocking polling completion. |
| **06** | Resilient SSE Streaming & Polling Fallback | `ferry-daemon`, Web SPA | **DONE** | Implemented `GET /api/events` returning Server-Sent Events stream with initial status emit. Browser gracefully falls back to 2s polling without console exceptions. |
| **07** | Minimalist Zero-Jargon Web Dashboard | `ferry-daemon/assets` | **DONE** | Complete overhaul into clean captive-portal style: Hero status banner (`SYNCED`, `HOLDING`, `CONFLICTS`, `OFFLINE`), live Activity Feed terminal, Connected Devices, Work Protection, light/dark theme toggle, and 390px mobile responsiveness. |
| **08** | Honest Token Authentication & Session Storage | `ferry-daemon`, Web SPA | **DONE** | Tokens cached in `sessionStorage` and sent as `Authorization: Bearer <token>`. Unauthenticated visits prompt with token modal. Footer declares honest security notice (`Localhost only · Protected by session token`). |
| **09** | End-to-End Live Process & Playwright Verification | Integration Tests | **DONE** | Dual-node live process sync tests, CLI pin duration & ignore targeting tests, and automated Playwright browser tests. |

---

## Live End-to-End Verification Results

### 1. Dual-Node Concurrent Unpinned Modification & Quarantine
- **Scenario**: Node A and Node B exchange and achieve initial agreement on `shared.txt`. Both unpinned nodes concurrently modify `shared.txt`.
- **Observed Behavior**: Ferry's three-way reconciler evaluated both modifications against the common base manifest. The winning modification was kept in place, the losing revision was preserved in `shared.txt.ferry-conflict.<device>-<timestamp>`, and an immutable entry was written to `.ferry/conflicts.jsonl`.
- **Verdict**: **PASSED** (Zero silent data loss).

### 2. Pinned Session Holding & Expiration
- **Scenario**: Run `ferry pin start --hours 8` on a watched project.
- **Observed Behavior**: Pin command transferred session ownership to daemon PID with `expires_sec` recorded. `ferry pin status` across separate CLI invocations verified active holding state without premature staleness.
- **Verdict**: **PASSED**.

### 3. External Directory Ignore Targeting
- **Scenario**: Run `ferry ignore --list <path>`, `ferry ignore "*.log" <path>`, and `ferry ignore --preset claude <path>` targeting an external folder.
- **Observed Behavior**: Rules and presets applied accurately to target folder's `ferry.ignore` and `.ferry/settings.json` without requiring `cd`.
- **Verdict**: **PASSED**.

### 4. Playwright Web Dashboard & Authentication Verification
- **Scenario**: Start daemon UI server with session token on port 8921. Access via browser with token, test UI interactions, theme switching, token storage, and mobile responsiveness.
- **Observed Metrics**:
  - Hero Status Header: `SYNCED` with illuminated green dot.
  - Subtitle: Plain English explanation ("Folder is up to date · Ready for pairing").
  - Theme Switching: Single click toggles `data-theme="light"` / `dark` and persists in `localStorage.ferry_theme`.
  - Session Persistence: Token extracted from URL query param, stored in `sessionStorage.ferry_token`, attached on subsequent calls, retained across page reloads without URL param.
  - Mobile Viewport (390 × 844 px): Layout collapsed to single column with `hasHorizontalScroll: false` (scrollWidth = 390, clientWidth = 390).
- **Verdict**: **PASSED**.

---

## Automated Test Summary

```text
cargo test --workspace
  - ferry-sync: 25 unit tests passed, integration suites (bootstrap, convergence, incremental_index, integrity) passed
  - ferry-store: 122 unit tests passed
  - ferry-pin: 13 unit tests + scenarios passed
  - ferry-ipc: 12 unit tests passed
  - ferry-daemon: 15 unit tests + server tests passed
  - ferry-cli: 77 unit & integration tests passed (including live_verification_e2e)
  - ferry-tui: 34 render and event tests passed
  - Total: 300+ tests passed, 0 failures, 0 warnings

cargo clippy --workspace --all-targets -- -D warnings
  - Clean build across all 15 crates with 0 warnings.
```
