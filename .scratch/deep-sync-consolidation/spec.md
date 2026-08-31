# Spec: Deep Sync Engine & Backend Architecture Consolidation

## Problem Statement

Developers, operators, and automated coding agents using Ferry experience architectural friction, latency, and reliability risks due to fragmented sync and backend subsystems:
1. **Split Sync Subsystems & Sleep Polling**: Sync orchestration is split between two separate crates (`ferry-sync` for network/poll loop and `ferry-sync-engine` for 3-way transactional convergence). The background sync loop uses an un-debounced 200ms sleep poll calling raw directory snapshotting, bypassing native OS filesystem watchers (`ScanEngine`) and adding unnecessary CPU churn and latency.
2. **Ignore Policy Disconnect**: Because the sync loop calls raw snapshotting rather than routing through `ScanEngine`, user-configured ignore rules (e.g. `node_modules`, build artifacts, `.env`) are ignored during live sync polling.
3. **Leaky Session Pinning Choreography**: Callers coordinating sync exchanges must manually orchestrate matcher loading, closure generation, convergence, and ledger persistence across 4 discrete steps. Any omission of the persistence step causes held edits to vanish on restart.
4. **Redundant IPC Adapters**: Frontends (`ferry-gui`, `ferry-tui`, CLI) duplicate pass-through adapters (`DaemonIpcAdapter`) and hardcode bifurcated fallback routing rather than consuming a single deep `UiBackend` interface.
5. **Supervisor Bootstrap & Crash Recovery Overhead**: The daemon supervisor manually coordinates a 10-step procedural engine bootstrap and relies on periodic health-flag polling rather than self-healing engines with direct event streaming.

## Solution

Consolidate Ferry's sync orchestration, change detection, pinning, and UI backend layers into deep, high-leverage modules:
1. **Unified Deep Sync Engine**: Merge sync orchestration and transactional convergence into a single deep engine subsystem. The engine embeds `ScanEngine` as its native change detector (subscribing to OS watcher events), evaluates `FerryIgnore` at the scan seam, and executes 3-way convergence in-process without sleep polling.
2. **Authoritative Ignore Binding at the Scan Seam**: Ensure all manifest generation during sync passes through `ScanEngine` bound to the folder's compiled `FerryIgnore` policy.
3. **Transactional Convergence Pinning**: Make `ConvergenceEngine` natively aware of session pin state, gating changes and atomically persisting `HeldLedger` in a single transactional step. Provide a clean `PinManager` lifecycle interface.
4. **Universal UI Backend Adapter**: Purge `DaemonIpcAdapter` and provide `AutoBackend` directly in `ferry-ipc::backend` as the single universal adapter with consistent local/remote fallback across all frontends.
5. **Self-Healing Folder Engine**: Encapsulate folder engine initialization, exponential backoff crash recovery, and real-time `UiEvent` broadcast streaming within `FolderEngine`.

## User Stories

1. As a developer editing code in a synced folder, I want file system changes to be picked up instantly via OS file watchers rather than an un-debounced 200ms sleep loop, so that synchronization has sub-millisecond local latency with minimal CPU idle load.
2. As a developer with `node_modules` or `.env` in my ignore rules, I want active sync sessions to strictly respect these rules during background sync, so that private secrets and large build artifacts never synchronize over the wire.
3. As an operator running `ferry daemon`, I want the daemon supervisor to manage sync engines through a single deep interface, so that store opening, polynomial derivation, and event publishing are consistent across all folders.
4. As an AI agent working with session pinning on device A, I want held remote edits to be transactionally ledgered in `.ferry/held/` on every sync exchange, so that my local work in progress is never overwritten and held changes survive daemon restarts.
5. As a developer releasing a session pin, I want all accumulated held changes to converge through a single transactional release call, so that remote changes are applied cleanly without manual ledger bookkeeping.
6. As a GUI user, I want the GUI backend to automatically connect to the daemon over IPC and seamlessly fall back to local disk operations when the daemon is offline, so that I experience consistent folder management in all environments.
7. As a TUI user, I want the terminal interface to consume the universal backend adapter without duplicate adapter boilerplate, so that features and bug fixes behave identically between GUI and TUI.
8. As an engineer writing integration tests, I want to test sync behavior through a single `SyncEngine` seam with `FakeBackend`, so that test suites run instantaneously without relying on sleep timers or thread polling.
9. As a developer extending sync transports, I want `SyncEngine` to manage transport sessions cleanly without leaking connection lifetimes or thread handles.
10. As an operator monitoring daemon activity, I want `FolderEngine` to stream structured `UiEvent` updates (state changes, transfers, errors) directly over broadcast channels, so that dashboards receive instant progress feedback.
11. As a developer reviewing the codebase, I want `ferry-materialize` to focus solely on atomic crash-safe file writes without obsolete traits like `PinGate`, so that module responsibilities are clear and deep.
12. As a security-conscious user, I want trust policies and key exchange rules to be enforced uniformly at the wire session boundary across both direct P2P and relay connections.

## Implementation Decisions

1. **Consolidated Deep `SyncEngine` Module**:
   - Merge the wire exchange loop from `ferry-sync` with `ferry-sync-engine` into a single deep crate/module.
   - Embed `ScanEngine` (with notify watcher integration and debounce logic) directly within `SyncEngine`.
   - Eliminate raw calls to `ferry_store::snapshot_dir_incremental` in sync loops, making `ScanEngine` the exclusive seam for local manifest updates.
   - Delete the obsolete `PinGate` trait from `crates/ferry-materialize/src/apply.rs`.

2. **Authoritative Ignore Binding**:
   - `open_folder` in `ferry-folder` automatically loads and compiles `FerryIgnore` rules.
   - `ScanEngine` accepts `Arc<dyn IgnorePolicy>` and enforces filtering during directory walks and watcher event processing.

3. **Transactional Pinning in `ConvergenceEngine`**:
   - Update `ConvergenceEngine` to accept folder state configuration and automatically check active pins, gate conflicting entries, write to `HeldLedger`, and execute atomic materialization.
   - Provide `PinManager::release` which executes convergence, verifies disk application, and clears the ledger in one transactional operation.

4. **Universal `UiBackend` in `ferry-ipc`**:
   - Delete `DaemonIpcAdapter` from `ferry-daemon::ui::backend`.
   - Implement `AutoBackend` in `ferry-ipc::backend` implementing `UiBackend`, wrapping `DaemonClient` with automated reconnection and in-process fallback.
   - Update `ferry-gui` and `ferry-tui` to construct `ferry_ipc::backend::connect_auto(socket, folder)`.

5. **Self-Contained `FolderEngine`**:
   - Introduce `FolderEngine` in `ferry-daemon` encapsulating store opening, polynomial checks, background worker threads/tasks, internal restart backoff, and direct broadcast `Sender<UiEvent>` event streaming.
   - Simplify `Supervisor` into a registry map: `HashMap<FolderId, FolderEngine>`.

## Testing Decisions

- **Test External Behavior**: Test observable behavior across high-level seams (file synchronization, watcher event handling, ignore rule enforcement, held ledger recovery, and typed UI event streaming) rather than internal helper states or sleep intervals.
- **Seams**:
  - **Sync Engine Seam**: Test `SyncEngine` against mock network transports and filesystem edits, verifying that dirty files trigger immediate debounce sync and manifest convergence.
  - **Ignore Policy Seam**: Test end-to-end sync sessions verifying that paths matching `ferry.ignore` or `.ferry/settings.json` never appear in peer manifests or disk writes.
  - **Session Pinning Seam**: Test multi-device convergence during active pin, verifying `.ferry/held/<peer>.jsonl` persistence, daemon restarts, and clean convergence upon release.
  - **Universal UI Backend Seam**: Test `AutoBackend` with and without active daemon socket using `contract_tests.rs` and `gui_tests.rs`.
- **Prior Art**:
  - `crates/ferry-sync-engine/tests/matrix.rs`
  - `crates/ferry-scan/tests/scan_tests.rs`
  - `crates/ferry-pin/tests/`
  - `crates/ferry-ipc/tests/contract_tests.rs`
  - `crates/ferry-daemon/tests/supervisor_tests.rs`

## Out of Scope

- Changes to wire protocol serialization format (`ferry-proto` bincode framing).
- Cryptographic primitives changes (ChaCha20-Poly1305, BLAKE3, Ed25519).
- Cloud relay server protocol changes.
- Web UI frontend redesign.

## Further Notes

This spec synthesizes the 5 candidates surfaced during the architectural review. Implementing this task graph will eliminate polling sleep loops, unify change detection, ensure ignore policy compliance, and remove redundant layers across frontends.
