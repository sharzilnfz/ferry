# Issue 6: Clean up compiler dead-code and unused-import warnings

Status: ready-for-agent
Feature: `live-testing-fixes`
Depends on: .scratch/live-testing-fixes/issues/05-cli-web-token-query-command.md
Blocks: 

## Context
Building `ferry-cli` and related workspace crates triggers several compiler warnings across platforms:
- `crates/ferry-platform/src/time.rs`: `civil_from_days` and `days_from_civil` dead code warnings.
- `crates/ferry-platform/src/winpath.rs`: `EXTENDED_PREFIX` and `EXTENDED_UNC_PREFIX` dead code warnings.
- `crates/ferry-ipc/src/backend.rs`: Unused imports (`RwLock`, `std::collections::HashMap`, `DirectoryEntry`, `sort_entries`).
- `crates/ferry-daemon/src/supervisor/engine.rs`: Unused imports (`EngineConfig`, `SyncEngine`).

## Target Files
- `crates/ferry-ipc/src/backend.rs`
- `crates/ferry-daemon/src/supervisor/engine.rs`
- `crates/ferry-platform/src/time.rs`
- `crates/ferry-platform/src/winpath.rs`

## Requirements
1. Remove all unused imports across `ferry-ipc` and `ferry-daemon`.
2. For platform helpers in `ferry-platform` that are used conditionally or in tests on other OSes, annotate with `#[allow(dead_code)]` alongside explanatory comments, or remove if truly obsolete.
3. Verify that `cargo check --all-targets` and `cargo build --workspace` produce 0 warnings.
