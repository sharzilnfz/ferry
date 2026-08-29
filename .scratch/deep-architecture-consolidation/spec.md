# Feature Specification: Deep Architecture Consolidation

Status: ready-for-agent

## Problem Statement

Developers and maintainers experience architectural friction when adding features, debugging state mutations, and running frontend interfaces across the codebase.

1. **Folder Inventory Fragmentation.** Managing registered project folders and inspecting the filesystem is split across four separate modules. Frontends call directory listing logic that re-reads and re-parses storage files independently to compute sync status, while registration validation and overlap checks reside in a separate crate.
2. **Shallow Frontend RPC Seam.** The frontend interface exposes sixteen granular methods where every single operation opens, handshakes, and terminates a fresh socket connection. Four distinct adapters maintain hundreds of lines of pass-through boilerplate, making connection drops and event subscriptions brittle.
3. **Bifurcated Pairing Mechanics.** Device pairing is split into two disjoint code paths: out-of-band file payload creation and in-band short-code rendezvous. Callers must know which transport is active rather than expressing the intent to pair.
4. **Intermediate Convergence Translation.** The sync engine computes an intermediate action plan and passes it to an execution adapter that translates each action into individual materializer calls, scattering atomic rollback guarantees.

## Solution

Deepen the codebase architecture by consolidating shallow wrappers and fragmented responsibilities into cohesive, high-leverage modules placed at clear seams.

1. **Deep Folder Inventory Module.** Unify folder registration, persistence, atomic file locking, path traversal guards, git status detection, and directory inspection inside a single deep module.
2. **Multiplexed Daemon Client Seam.** Collapse the sixteen-method frontend trait into high-level domain sessions backed by a single persistent, multiplexed connection with automatic reconnect and in-memory test substitution.
3. **Unified Pairing Ritual.** Encapsulate short-code rendezvous and payload file exchange behind a single pairing engine that negotiates the optimal transport transparently.
4. **Atomic Convergence Engine.** Fuse three-way reconciliation, atomic file materialization, conflict quarantine, and agreement ledger commits into a single transactional convergence operation.

## User Stories

1. As a developer adding a new folder to sync, I want the path validated, canonicalized, and checked for overlap atomically, so that invalid or duplicate directory registrations are rejected before touching disk.
2. As a GUI or TUI user browsing directories in the folder picker, I want folder metadata (such as git repositories and already synced status) computed in a single pass, so that directory listings are instant with zero redundant file reads.
3. As a developer running the desktop GUI, I want the client to maintain a persistent connection to the background daemon, so that state queries and event subscriptions do not incur per-action socket handshake latency.
4. As a maintainer debugging folder state, I want all folder registry modifications to flow through one module, so that disk persistence, sorting, and validation invariants cannot drift.
5. As an engineer writing frontend tests, I want to substitute the entire daemon communication seam with a fast in-memory fake, so that UI behavior is verified deterministically without spinning up local socket servers.
6. As a developer pairing two devices, I want to provide either a 6-character code or a payload file through the same interface, so that the application handles key exchange and transport selection without exposing protocol mechanics.
7. As a mobile or desktop user on a local network, I want short-code pairing to establish an encrypted session automatically, so that I do not need to manually move files across machines.
8. As a developer syncing divergent file trees, I want three-way reconciliation, atomic temp-file materialization, conflict quarantine, and agreement ledger writes to execute as a single atomic unit, so that an interrupted sync cycle leaves the working tree consistent.
9. As a developer running automated test suites, I want sync convergence tests to execute through the top-level convergence seam, so that tests verify real filesystem outcomes rather than intermediate action plans.
10. As a headless CLI user, I want the application to automatically choose between in-process execution and daemon IPC without leaking adapter boilerplate into command handlers.

## Implementation Decisions

### 1. Folder Inventory Module

Consolidate `ferry-ipc::fs`, `ferry-ipc::registry`, `ferry-daemon::registry`, and `ferry-folder` into a deep `FolderInventory` module.

- **Interface.** The module presents a compact interface for registering folders, unregistering folders, querying active records, and inspecting filesystem directories.
- **Encapsulated Behavior.** The module implementation absorbs atomic TOML persistence at the device home path, file locking, path traversal guards, NFC unicode normalization, duplicate/overlap detection, git repository detection, and sync status calculation.
- **Deletion.** Delete the shallow `FolderRecord` and `FolderRegistry` structs in `ferry-ipc::registry` and the ad-hoc TOML parser in `ferry-ipc::fs`.

### 2. Multiplexed Daemon Client and Session Seam

Deepen the frontend communication seam in `ferry-ipc` and `ferry-daemon`.

- **Interface.** Replace the 16-method `UiBackend` trait with a structured `DaemonClient` presenting three cohesive session domains: status/telemetry streaming, folder inventory operations, and pairing/pinning lifecycle.
- **Encapsulated Behavior.** The production adapter maintains a persistent background connection over the local socket or named pipe, handling request/response multiplexing, push event fanout, auto-reconnect backoff, and transparent in-process fallback.
- **Deletion.** Delete per-method socket connect/disconnect blocks in `DaemonIpcAdapter` and remove 200 lines of pass-through delegation in `AutoBackend`.

### 3. Unified Device Pairing Ritual

Consolidate `ferry-folder::pairing`, `ferry-crypto::pairing_code`, and `ferry-sync::pairing_transport`.

- **Interface.** The pairing engine exposes a high-level ritual interface: generate an offer (producing both a 6-character short code and optional sealed payload envelope) and accept an offer (taking either code or payload).
- **Encapsulated Behavior.** The module internally manages key derivation, AEAD envelope construction, QR payload generation, timeout expiration, and transport selection (QUIC hole punching, relay, or file exchange).

### 4. Transactional Convergence Engine

Unify `ferry-sync-engine` and `ferry-materialize`.

- **Interface.** Callers invoke a single convergence function taking the local tree, remote manifest, base manifest, and store reference.
- **Encapsulated Behavior.** The engine executes three-way diffing, fetches required blobs, applies atomic temp-file renames, writes quarantined loser files with deterministic suffixes, logs conflict entries, and commits the agreement ledger in a single transactional step.
- **Deletion.** Delete the intermediate `ActionPlan` translation loop in `execute.rs`.

## Testing Decisions

- **Test Through the Highest Seam.** Tests interact exclusively through the public module interfaces (`FolderInventory`, `DaemonClient`, `PairingRitual`, `ConvergenceEngine`). Tests do not assert on internal intermediate structs or private helper states.
- **Local-Substitutable Stand-Ins.** The `DaemonClient` seam provides an in-memory fake backend adapter satisfying the exact session contracts for UI component tests.
- **Prior Art.** Follows existing test patterns in `crates/ferry-crypto/src/pairing_code_tests.rs`, `crates/ferry-sync-engine/src/reconcile_tests.rs`, and `crates/ferry-ipc/src/transport_tests.rs`.

## Out of Scope

- Changes to wire protocol serialization formats or chunking algorithms.
- Introduction of cloud databases or remote user accounts.
- Changes to desktop GUI widget styling or visual themes.

## Further Notes

- All changes maintain strict compatibility with existing CLI commands and UI frontends.
- Code changes preserve immutability and zero-unsafe invariants across all crates.
