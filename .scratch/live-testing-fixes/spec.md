# Spec: Unified Live Testing & Verification Remediation

Status: ready-for-agent
Feature Slug: `live-testing-fixes`
Date: 2026-08-31
Sources:
- Live verification recipe maintenance pass (`/maintain-verification-skill` & Issue T-016)
- Dual-device live testing suite (macOS MacBook Air $\longleftrightarrow$ Arch Linux over Tailscale)

---

## Executive Summary

During live multi-device and end-to-end integration testing of Ferry across macOS and Arch Linux, six critical functional bugs, UX gaps, and hygiene issues were discovered across the synchronization exchange engine, pairing protocol, terminal user interface (TUI), CLI toolchain, and daemon web server.

This unified specification consolidates all reported live testing defects into a single, cohesive remediation plan and sequential ticket queue:

1. **Held Manifest Persistence**: Daemon neglects to persist incoming un-adopted remote manifests into `.ferry/store/` when session pins hold changes, causing `ferry pin release` to crash with `held-manifest-missing`.
2. **Persistent Multi-Process Short-Code Pairing**: `ferry share` stores pairing offers in an ephemeral in-process memory map and exits, causing separate `ferry join` processes to fail with `pairing-not-found`, while failing to update the sharer's `CONFIG_HEAD` allow-list.
3. **TUI Pin Toggle State Detection**: In the TUI, pressing `P` when a pin is active but no files are currently being held attempts to start another pin instead of releasing/stopping the active pin, throwing `pin-active` errors.
4. **TUI Disconnected Daemon Log Spam**: Running `ferry tui` without an active daemon triggers rapid reconnect attempts that flood the activity feed with `Backend event stream closed` errors.
5. **Web UI Token Discovery Command**: The one-time Web UI authentication token is only printed to stdout on startup; backgrounded or headless sessions cannot inspect or retrieve the token.
6. **Compiler Warning and Dead Code Cleanup**: Unused imports and unused platform helper functions generate compiler warnings during workspace builds.

---

## Architecture & Seams

Remediations leverage existing domain boundaries and the highest observable seams:

| Domain | Target Seam | What Is Tested Through It | Key Interfaces |
|---|---|---|---|
| **Pin Manifest Storage** | `ferry-sync::exchange::Exchange` & `ferry-sync-engine::converge::ConvergenceEngine` | Storing un-adopted remote manifest bytes into `Store` during held sync exchanges | `store.put_meta(BlobKind::Manifest, &bytes)`, `PinManager::release_peer` |
| **Pin Release & Quarantine** | `ferry-sync-engine::pin::PinManager` & `ferry-cli::commands::pin` | 3-way reconciliation, conflict file quarantine (`*.ferry-conflict.*`), ledger logging, held cleanup | `PinManager::release`, `.ferry/conflicts.jsonl`, `.ferry/held/` |
| **Cross-Process Pairing** | `ferry-folder::pairing::PairingRitual` & `ferry-cli::commands::share`/`join` | Filesystem-backed pairing rendezvous, code discovery, mutual key wrapping in `CONFIG_HEAD` | `$TMPDIR/ferry-rendezvous/`, `CONFIG_HEAD` allow-list |
| **TUI Key Handling & State** | `ferry-tui::app::handle_key_inner` & `ferry-tui::state::FolderViewState` | Toggle pin lifecycle (`active`/`pinned` $\rightarrow$ `ReleasePin`), backend disconnect backoff | `FolderViewState::pin`, `ClientCommand::ReleasePin` |
| **Web UI Session Discovery** | `ferry-daemon::ui::server` & `ferry-cli::commands::ui` | Persisting active session metadata (`web_session.json`) and querying URL/token via CLI | `.ferry/web_session.json`, `ferry ui token` |
| **Workspace Hygiene** | `ferry-platform`, `ferry-ipc`, `ferry-daemon` | Zero compiler warnings on `cargo check --all-targets` and `cargo build` | Code pruning, `#[allow(dead_code)]` |

---

## User Stories

### 1. Session Pinning & Held Manifest Reconciliation
- **US-1.1**: As a developer editing code on Device A with an active session pin (`ferry pin start --paths '<glob>'`), I want incoming remote changes held by the daemon to have their remote manifest bytes safely stored in `.ferry/store/`, so that later reconciliation commands have full access to the incoming snapshot.
- **US-1.2**: As a developer running `ferry pin release`, I want held modifications evaluated and reconciled without encountering `held-manifest-missing` errors.
- **US-1.3**: As a developer releasing a pin with conflicting edits, I want non-conflicting held changes applied to my tree and conflicting changes quarantined as `<file>.ferry-conflict.<device_short>-<timestamp>`, recorded in `.ferry/conflicts.jsonl`.
- **US-1.4**: As an automated script checking pin status, I want `ferry pin status --json` to report `{"held_changes": 0, "holding": false}` after release completes.

### 2. Multi-Process Short-Code Pairing
- **US-2.1**: As a developer running `ferry share` in Terminal 1, I want a 6-character short code (e.g. `7KQ4-2M`) that can be used by `ferry join 7KQ4-2M ~/dest` in Terminal 2 across separate processes without throwing `pairing-not-found`.
- **US-2.2**: As a developer completing short-code pairing, I want Device A's `CONFIG_HEAD` to automatically append Device B's wrapped Folder Master Key (FMK) grant, so that background daemons immediately authorize each other.
- **US-2.3**: As a developer pairing devices, I want short codes to expire cleanly after their validity window and be securely removed once consumed.

### 3. TUI Usability & Disconnected Resilience
- **US-3.1**: As a developer using `ferry tui`, I want pressing `P` when a pin is active (even if `holding` is currently `false`) to release/stop the pin rather than attempting to start a duplicate pin and logging error messages.
- **US-3.2**: As a developer launching `ferry tui` before the daemon is running, I want the TUI to display a clear `DISCONNECTED` status and throttle reconnection attempts with exponential backoff, rather than flooding the Recent Activity log with error spam.

### 4. Headless & Background Web UI Token Discovery
- **US-4.1**: As a developer running `ferry ui --web` in the background or on a remote machine, I want to run `ferry ui token [folder]` to retrieve the full URL with the valid authentication token (`http://127.0.0.1:<port>/?token=<token>`).
- **US-4.2**: As a developer checking Web UI status when no server is running, I want `ferry ui token` to return a clear error code (`no-active-web-ui`).

### 5. Build Hygiene & Warning-Free Compilation
- **US-5.1**: As a maintainer building the workspace across macOS and Linux, I want `cargo check --all-targets` and `cargo test` to execute with 0 compiler warnings.

---

## Detailed Implementation Plan

### Area 1: Held Manifest Storage & Pin Release (Issue 01)
- In `ferry-sync::exchange::Exchange::exchange_folder()`:
  - When `outcome.held > 0`, persist remote manifest bytes to store: `self.store.put_meta(BlobKind::Manifest, &man_bytes)?`.
  - Maintain the existing invariant that working tree files and `AgreementLedger` remain untouched while holding.
- In `ferry-sync-engine::pin::PinManager::release_peer()`:
  - Retrieve the held remote manifest from the store using the recorded manifest hash.
  - Perform 3-way reconciliation against baseline and local manifest.
  - Write non-conflicting changes to working directory, quarantine conflicts as `<path>.ferry-conflict.<device_short>-<timestamp>`, append to `.ferry/conflicts.jsonl`, delete `.ferry/held/<peer>.jsonl`, and transition pin state to ended.

### Area 2: Persistent Rendezvous & Allow-List Wrap for Short-Code Pairing (Issue 02)
- In `ferry-folder::pairing::PairingRitual`:
  - Provide a filesystem-backed rendezvous storage provider (under `$TMPDIR/ferry-rendezvous/` or `$FERRY_HOME/rendezvous/`) for local cross-process pairing sessions.
  - In `ferry-cli::commands::share`:
    - Ensure the share command waits for joiner acceptance (with timeout) or coordinates with the local daemon/rendezvous watcher.
    - Upon joiner acceptance, extract joiner's device public key, compute Folder Master Key wrap, append the new grant to `CONFIG_HEAD`, and atomically commit `CONFIG_HEAD`.
  - In `ferry-cli::commands::join`:
    - Read offer from persistent rendezvous, submit response, and receive/decrypt FMK.

### Area 3: TUI Pin Toggle Logic (Issue 03)
- In `crates/ferry-tui/src/app.rs` (`handle_key_inner` for `KeyCode::Char('p' | 'P')`):
  - Check `self.state.pin.is_active()` or `self.state.pin.state != "none"`.
  - If active, dispatch `KeyOutcome::Command(ClientCommand::ReleasePin)`.
  - If inactive (`state == "none"`), dispatch `KeyOutcome::Command(ClientCommand::StartPin { ... })`.

### Area 4: TUI Disconnected Stream Throttling & Log Deduplication (Issue 04)
- In `crates/ferry-tui/src/app.rs`:
  - When the event receiver encounters disconnect/EOF, apply backoff delay (1s $\rightarrow$ 2s $\rightarrow$ 5s) before reconnecting.
  - Deduplicate consecutive disconnect errors so only a single entry appears in the activity list.
  - In `crates/ferry-tui/src/ui.rs`, display a `DAEMON DISCONNECTED` badge in the header when offline.

### Area 5: Web UI Session Metadata & Token Query CLI (Issue 05)
- In `crates/ferry-daemon/src/ui/server.rs`:
  - On web server start, write `{ "port": port, "host": host, "token": token, "pid": pid, "created_at": ... }` to `.ferry/web_session.json`.
  - On shutdown or drop, delete `.ferry/web_session.json`.
- In `crates/ferry-cli/src/commands/ui.rs` & `crates/ferry-cli/src/cli.rs`:
  - Add `ferry ui token [folder]` subcommand / `--token` flag.
  - Read `.ferry/web_session.json`, verify process liveness, and output `http://<host>:<port>/?token=<token>`.
  - If file does not exist or process is dead, output structured error `code: "no-active-web-ui"`.

### Area 6: Dead Code & Warning Pruning (Issue 06)
- In `crates/ferry-ipc/src/backend.rs`: Remove unused imports (`RwLock`, `HashMap`, `DirectoryEntry`, `sort_entries`).
- In `crates/ferry-daemon/src/supervisor/engine.rs`: Remove unused imports (`EngineConfig`, `SyncEngine`).
- In `crates/ferry-platform/src/time.rs` & `winpath.rs`: Annotate platform-conditional helpers with `#[allow(dead_code)]` with explanatory comments.

---

## Verification & Acceptance Criteria

1. **Unit & Integration Test Suites**:
   - `cargo test --workspace` passes cleanly.
   - `cargo check --all-targets` produces 0 compiler warnings.
2. **Pin Release Test**:
   - Pin start on Device A $\rightarrow$ remote edit from Device B held $\rightarrow$ `ferry pin release` reconciles held manifest $\rightarrow$ quarantine file produced $\rightarrow$ zero crashes.
3. **Cross-Process Share/Join Test**:
   - Process 1 `ferry share` $\rightarrow$ Process 2 `ferry join <CODE>` $\rightarrow$ both daemons converge without handshake authorization denials.
4. **TUI Key & Disconnect Tests**:
   - Unit tests verify `P` toggles `ReleasePin` on active pin.
   - Disconnected backend test verifies throttled reconnection and clean UI.
5. **CLI Token Query Test**:
   - Web UI start $\rightarrow$ `ferry ui token` prints URL with token $\rightarrow$ Web UI stop $\rightarrow$ `no-active-web-ui`.

---

## Unified Ticket Index

All unified remediation issues reside in `.scratch/live-testing-fixes/issues/`:

| Ticket | Title | Depends On | Blocks |
|---|---|---|---|
| `01-held-manifest-pin-release.md` | Persist un-adopted remote manifests into Store during daemon holds | — | `02` |
| `02-short-code-pairing-rendezvous.md` | Persistent multi-process rendezvous and CONFIG_HEAD wrap for pairing | `01` | `03` |
| `03-tui-pin-toggle-active-state.md` | Fix TUI pin toggle to release active pin when holding is false | `02` | `04` |
| `04-tui-backend-disconnected-handling.md` | Prevent TUI activity log spam when daemon is disconnected | `03` | `05` |
| `05-cli-web-token-query-command.md` | CLI command / helper to retrieve active Web UI URL and token | `04` | `06` |
| `06-compiler-warnings-and-dead-code-cleanup.md` | Clean up compiler dead-code and unused-import warnings | `05` | — |
