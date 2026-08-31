# Spec: Code Review Findings & Architecture Hardening

## Problem Statement

During the architecture-deepening code review, several code smells and boundary irregularities were identified across the workspace:
1. **Scattered domain error strings and remediation advice**: Uninitialized folder validation messages and remediation hints (`"run ferry init or ferry pair"`) are hardcoded separately across the TUI, GUI, and Web UI server rather than originating from a single canonical error definition in the folder module.
2. **Channel type pollution in GUI event dispatch**: Successful folder registration is broadcast over the `UiEvent::Error` channel using a magic string code (`"folder_registered"`), conflating errors and success events.
3. **Duplicated engine spawning in ad-hoc CLI daemon mode**: When `--listen` or `--peer-url` CLI arguments are supplied, the CLI daemon command duplicates store opening, chunker polynomial validation, and engine startup logic rather than reusing the centralized supervisor infrastructure.
4. **Transport runtime boundary encapsulation**: The background Tokio runtime isolation in the Iroh QUIC transport layer needs clean boundary encapsulation and lifecycle management to prevent ad-hoc runtime nesting.

## Solution

1. **Centralize folder initialization errors and hints in `ferry-folder`**: Provide a canonical `FolderError` variant and formatted message/hint helper in the folder module, consumed by all UI surfaces (TUI, GUI, Web UI) and the daemon.
2. **Introduce explicit typed UI events for folder registration**: Replace the overloaded `UiEvent::Error` variant with a dedicated `UiEvent::FolderRegistered { path }` or structured state event in the IPC/UI event model.
3. **Consolidate ad-hoc CLI daemon engine startup through the Supervisor**: Route single-folder and multi-folder daemon commands through the unified supervisor and engine opening interface.
4. **Harden transport runtime isolation**: Formalize the background execution boundary for Iroh endpoints so blocking transport operations remain fully encapsulated.

## User Stories

1. As a CLI and TUI user, I want clear, consistent error messages when attempting to register an uninitialized folder, so that I always receive exact instructions on how to initialize or pair the folder.
2. As a GUI user, I want clear and consistent error feedback when picking an uninitialized directory, so that the application guides me to initialize the folder before syncing.
3. As a Web UI user, I want standard HTTP and JSON responses for uninitialized directory registrations, so that the UI accurately displays the initialization warning banner.
4. As a GUI user, I want folder registration success events to be distinct from error events, so that the UI activity log and notifications cleanly separate successes from failures.
5. As a developer maintaining the GUI, I want typed `UiEvent` variants for lifecycle events, so that I do not need to parse error codes to handle successful folder additions.
6. As a CLI user running `ferry daemon` with specific network flags, I want the daemon to initialize and supervise sync engines through the same verified pipeline as the standard daemon, so that polynomial handling, store security, and health monitoring behave identically.
7. As a developer extending sync transports, I want transport threads and async runtimes to be cleanly isolated, so that transport operations never panic due to nested Tokio runtimes.
8. As an operator inspecting logs, I want consistent error codes and domain hints across all frontends, so that troubleshooting sync setup is straightforward.

## Implementation Decisions

1. **Domain Error Export in `ferry-folder`**:
   - Define a dedicated `FolderError` constructor `not_initialized(path: &Path)` in `ferry-folder` that packages the canonical message `"not an initialized Ferry folder"` and remedy hint `"run 'ferry init' or 'ferry pair' before syncing this folder"`.
   - UI adapters and HTTP endpoints in `ferry-daemon`, `ferry-tui`, and `ferry-gui` will delegate formatting to this error definition instead of maintaining custom strings.

2. **Typed `UiEvent` in `ferry-ipc`**:
   - Add a distinct variant to `UiEvent` (e.g. `UiEvent::FolderRegistered { path: String }` or `UiEvent::Info { code: String, message: String }`).
   - Update `ferry-gui` event handlers and subscribers to process this typed event.

3. **CLI Daemon Engine Consolidation**:
   - Refactor `ferry-cli` daemon command execution to delegate engine creation directly to `ferry-daemon::supervisor::Supervisor` or unified `run_device_daemon` rather than manually constructing `EngineConfig` and calling `SyncEngine::with_store`.

4. **Transport Runtime Encapsulation**:
   - Ensure `ferry-iroh` transport manages its internal async runtime cleanly within its own struct boundary, providing synchronous trait methods (`dial`, `listen`) that do not leak runtime handles or spawn unmanaged threads.

## Testing Decisions

- **Test Quality Principle**: Test external observable behavior (rejection banners, typed IPC event delivery, CLI exit codes, and sync convergence) rather than internal function calls or thread states.
- **Seams**:
  - **High-level UI/IPC Seam**: Test that TUI and GUI state transitions and event streams receive typed registration events and canonical hint strings.
  - **HTTP API Seam**: Test `/api/registry/register` endpoint returning 409 Conflict with the exact domain error and hint.
  - **Supervisor/CLI Seam**: Test `ferry daemon` CLI invocation with `--listen` / `--peer-url` verifying store opening, chunker polynomial consistency, and clean teardown.
  - **Transport Seam**: Multi-endpoint discovery and direct/relay data transfer tests without runtime panics.
- **Prior Art**:
  - `crates/ferry-tui/tests/picker_tests.rs`
  - `crates/ferry-gui/tests/gui_tests.rs`
  - `crates/ferry-daemon/tests/server_fs_tests.rs`
  - `crates/ferry-daemon/tests/supervisor_tests.rs`
  - `crates/ferry-iroh/tests/roundtrip.rs`

## Out of Scope

- Changes to the underlying cryptographic algorithms (ChaCha20-Poly1305, BLAKE3).
- Wire protocol format modifications (`ferry-proto`).
- New GUI design themes or styling overhauls.
- Third-party relay hosting infrastructure.

## Further Notes

This spec addresses the code smells surfaced by the automated multi-agent code review pass on the `feat/architecture-deepening` branch, bringing all newly added code to full standard compliance.
