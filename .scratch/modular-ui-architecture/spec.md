# SPEC: Modular UI Architecture, Unified Core Seam & Native Pure-Rust GUI

Status: ready-for-agent  
Feature Slug: `modular-ui-architecture`  
Date: 2026-08-28  

## Problem Statement

Ferry currently has fragmented frontend implementations and tight coupling across presentation layers. The Web UI dashboard (`ferry-daemon/src/ui`) is unconditionally built into the daemon binary, pulling heavy HTTP and web dependencies even when running on headless servers or CI agents. Furthermore, the Web UI's backend fallback logic contains approximately 600 lines of duplicate disk-scanning, secret-checking, and pairing routines that duplicate code from `ferry-folder`, `ferry-pin`, and `ferry-scan`. The event stream in the Web UI uses a 1-second interval timer that polls and diffs JSON strings on every tick, causing continuous CPU wakeups. Finally, developers and agents wanting a desktop visual interface currently have to rely on an external web browser rather than an instantaneous, lightweight, native pure-Rust desktop GUI.

## Solution

Unify all user-facing frontends (Headless CLI, Terminal TUI, Web SPA Dashboard, and Native Pure-Rust Desktop GUI) behind a single deep asynchronous seam: the `UiBackend` trait. Implement two primary adapters: `DaemonIpcAdapter` (communicating over local IPC sockets/named pipes with zero filesystem re-scans) and `InProcessAdapter` (directly invoking `ferry-folder`, `ferry-scan`, and `ferry-sync-engine` in-memory without duplicate disk code). Gating frontends behind Cargo compile-time feature flags (`[features] web-ui, tui, gui, lean`) allows production servers to compile a stripped 4.2 MB headless binary. Replace all timer-based polling with zero-cost `tokio::sync::broadcast` stream subscriptions for true 0.0% idle CPU usage. Introduce `ferry-gui`, an `egui`/`eframe`-based native desktop client with sub-10ms startup and zero external browser dependencies.

## User Stories

1. As a developer running on a headless server, I want to compile Ferry with `--no-default-features --features lean`, so that the binary is stripped of all graphical and web dependencies and has a minimal disk footprint.
2. As a desktop developer, I want to launch `ferry ui --gui`, so that I get an instant native desktop window in under 10ms without launching a heavyweight browser tab.
3. As a developer using the Web UI, I want real-time server-sent events without background polling, so that my laptop achieves 0.0% idle CPU and preserves battery when no files are changing.
4. As a terminal user on an SSH session, I want to run `ferry ui --tui` or `ferry tui`, so that I can inspect sync state and manage session pins inside my terminal multiplexer.
5. As a developer using the CLI without the daemon running, I want `ferry status` and `ferry ui` to transparently scan the local folder in-process, so that I get immediate results without starting a background daemon process.
6. As a developer running `ferry daemon`, I want all connected frontends to query cached daemon memory over IPC, so that redundant disk rescans and hash recalculations are completely eliminated.
7. As an agent orchestrating sync sessions, I want machine-readable `--json` output across all commands, so that I can reliably parse state snapshots and conflict logs.
8. As a security-conscious developer, I want `share` operations to scan for accidental secrets before generating pairing payloads across both CLI and GUI interfaces, so that `.env` files and private keys are never shared by accident.
9. As a developer collaborating with AI coding agents, I want to hold incoming edits via session pinning in both the GUI and CLI, so that remote writes do not race the agent while it is generating code.
10. As a developer resolving concurrent edits, I want a structured conflict quarantine viewer in the native GUI and Web UI, so that I can inspect winners and quarantined loser files without data loss.
11. As a maintainer, I want a single unified `UiBackend` trait, so that adding a new frontend or modifying core folder logic requires touching only one interface seam.
12. As a developer attempting to run a feature-disabled frontend, I want clear and actionable error messages indicating which Cargo flag to enable, so that I can rebuild the tool quickly.
13. As a user operating in dark mode or low-light environments, I want the native GUI to match the obsidian fluid glass aesthetic of the web UI, so that visual hierarchy and state beacons are consistent everywhere.
14. As a Linux user without a desktop display manager, I want the CLI to cleanly reject `--gui` with a descriptive error while `--tui` and `--json` continue to work seamlessly.
15. As a Windows user in Developer Mode, I want IPC to communicate over named pipes with identical protocol parity to Unix domain sockets, so that frontends behave identically across all supported platforms.
16. As an accessibility-minded developer, I want clear color indicators accompanied by text labels and distinct shapes for Synced, Syncing, Holding, and Conflict states, so that status is unambiguous under all visual preferences.
17. As a developer running tests in CI, I want UI adapters to be testable with in-memory fakes, so that integration tests run in milliseconds without binding network ports or rendering graphics frames.
18. As a user transferring large project trees, I want live transfer progress bars and chunk counts in the GUI and TUI, so that I have immediate visibility into active peer synchronization.

## Implementation Decisions

1. **Unified Core Seam (`UiBackend` Trait)**:
   Extract the public backend contract into a deep trait:
   ```rust
   pub trait UiBackend: Send + Sync + 'static {
       fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>>;
       fn list_conflicts(&self) -> BoxFuture<'_, Result<Vec<ConflictEntry>, OpError>>;
       fn start_pin(&self, paths: Vec<String>, hours: Option<u64>) -> BoxFuture<'_, Result<PinRecord, OpError>>;
       fn stop_pin(&self) -> BoxFuture<'_, Result<PinStopSummary, OpError>>;
       fn release_pin(&self) -> BoxFuture<'_, Result<PinReleaseSummary, OpError>>;
       fn share_initiate(&self, folder: Option<PathBuf>, i_know: bool) -> BoxFuture<'_, Result<ShareOffer, OpError>>;
       fn share_status(&self, folder: Option<PathBuf>) -> BoxFuture<'_, Result<ShareStatus, OpError>>;
       fn pair_accept(&self, payload: PathBuf, dir: Option<PathBuf>) -> BoxFuture<'_, Result<PairResult, OpError>>;
       fn trigger_scan(&self) -> BoxFuture<'_, Result<(), OpError>>;
       fn subscribe_events(&self) -> BoxFuture<'_, Result<UiEventStream, OpError>>;
   }
   ```
   *(Origin: validated in architecture analysis and prototype)*.

2. **Concrete Adapter Implementations**:
   - `DaemonIpcAdapter`: Implements `UiBackend` by issuing typed `ClientCommand` messages to the daemon over IPC and consuming `DaemonMessage` broadcasts.
   - `InProcessAdapter`: Implements `UiBackend` by calling `ferry-folder`, `ferry-scan`, `ferry-pin`, and `ferry-sync-engine` directly. Deletes the ~600 lines of duplicate fallback logic previously in `ferry-daemon/src/ui/backend.rs`.
   - `AutoBackend`: Composite adapter that checks if the IPC socket is live; if connected, delegates to `DaemonIpcAdapter`; otherwise, delegates to `InProcessAdapter`.

3. **Cargo Feature Matrix**:
   - `default = ["web-ui", "tui", "gui"]`
   - `web-ui = ["dep:axum", "dep:tokio-stream"]`
   - `tui = ["dep:ratatui", "dep:crossterm", "dep:ferry-tui"]`
   - `gui = ["dep:eframe", "dep:egui", "dep:ferry-gui"]`
   - `lean = []` (minimal production build, stripping all UI crates).

4. **Runtime CLI Switch**:
   The `ferry ui` command accepts `--web`, `--gui`, or `--tui` (with `--gui` or `--web` as default based on compiled features). When a requested frontend is excluded at compile time, the command exits with code `feature-disabled` and hints the necessary Cargo build flag.

5. **Native Desktop GUI (`ferry-gui`)**:
   Implement a pure-Rust desktop client using `egui` and `eframe` (Glow/WGPU backends). Encapsulate custom obsidian glass theme tokens, pulsating beacon indicators, hairline telemetry strip, device fleet list, and modal dialogs.

6. **0.0% Idle CPU Event-Driven Engine**:
   Eliminate the 1-second polling timer in `DashboardServer`. All push notifications (SSE on web, repaint signals on GUI, message loop on TUI) subscribe directly to Tokio `broadcast::Receiver<DaemonMessage>` streams.

## Testing Decisions

- **What Makes a Good Test**: Tests must cross the public `UiBackend` seam or the CLI boundary, asserting observable domain outputs (`EngineSnapshot`, JSON schema compliance, error hints) rather than internal thread synchronization or private struct layouts.
- **Modules Tested**:
  - `DaemonIpcAdapter`: Test command serialization, response handling, and live broadcast event streams over in-memory or loopback duplex streams.
  - `InProcessAdapter`: Test direct status reads, secret scanning, pin management, and conflict list extraction on fixture folders.
  - `AutoBackend`: Test seamless automatic switching between IPC daemon and in-process fallback modes.
  - `ferry ui` CLI parsing: Test feature gating dispatch and error messages when features are disabled.
- **Prior Art**:
  - `crates/ferry-cli/tests/ipc_cli_integration.rs` (testing IPC command dispatch).
  - `crates/ferry-daemon/tests/ipc_server_tests.rs` (testing daemon IPC server responses).
  - `crates/ferry-tui/tests/render_tests.rs` (testing TUI state transitions and frame rendering).

## Out of Scope

- Deleting existing Web UI assets or endpoints (all existing web dashboard functionality is preserved).
- Mobile application ports (iOS/Android) or hosted SaaS dashboards.
- Multi-user authentication beyond the existing 32-character bearer token.
- Modifying wire synchronization protocols or store chunking algorithms (which remain governed by ADR-0001 through ADR-0005).

## Further Notes

- Interactive architecture documentation and benchmark comparisons are published at [`docs/architecture/interactive-architecture-report.html`](file:///Users/sharzilnafis/Projects/dumps/idea2/docs/architecture/interactive-architecture-report.html).
- The transition adheres strictly to the `/ponytail` ladder by deleting duplicate routines, preferring standard library and existing crate primitives, and avoiding speculative abstractions.
