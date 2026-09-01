# Ticket 05: Store AgreementLedger Path Sanitization and IPC Command Dispatch Deduplication

Status: completed
Depends on:
Blocks: 10

## What to build

Address two code hygiene and domain boundary findings from the standards review:

1. **Store `AgreementLedger` Path Sanitization**:
   - In `crates/ferry-store/src/agreement.rs` (`AgreementLedger::get` and `list_folder`), remove heuristic string matching (`dir.to_string_lossy().contains("/.ferry/")`) which leaks filesystem assumptions into the content-addressed store.
   - Require callers to construct `AgreementLedger` with the explicit store directory (`store.store_dir()`) or pass the resolved store root directly.

2. **Unify IPC Command Dispatch Cascades**:
   - In `crates/ferry-daemon/src/ipc/mod.rs`, `dispatch_client_command` and `dispatch_supervisor_command` contain duplicated `match` arms for `ListFolders`, `RegisterFolder`, `RemoveFolder`, `GetStatus`, `Ping`, `ListDirectory`, `CreatePairingSession`, and `JoinPairingSession`.
   - Extract common command execution into a shared dispatcher function `dispatch_common_command`, keeping only supervisor-specific or backend-specific operations in specialized match arms.

## Acceptance

- [x] `crates/ferry-store/src/agreement.rs` contains zero substring inspections on file paths.
- [x] IPC command handling is deduplicated into a single shared dispatcher.
- [x] `cargo test -p ferry-store` and `cargo test -p ferry-ipc` pass cleanly.

## Comments

Resolves Standards finding #4 (repeated switches) and #5 (store path parsing).
