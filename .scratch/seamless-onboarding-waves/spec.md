# Feature Specification: Seamless Onboarding & In-UI Directory Picker — Wave-Parallel Edition

Status: ready-for-agent
Feature Slug: `seamless-onboarding-waves`
Date: 2026-08-28
Supersedes: `.scratch/seamless-folder-picker/spec.md` (retained for reference; its ticket graph was serial and is replaced by this wave-parallel plan)

## Problem Statement

Same as the original. Setting up Ferry currently requires deep distributed-systems knowledge.

1. **Manual File-Based Pairing.** Trust between devices needs three files (`pair-offer`, `pair-response`, `pair-grant`) moved via `scp`.
2. **Explicit Network Flags.** Users must discover IPs, pick ports, configure `--listen` / `--peer-url`.
3. **Folder-Locked Daemon.** One daemon per folder, one terminal pane per folder.
4. **No In-UI Navigation.** No directory explorer in TUI, Web, or GUI. Users must type exact paths before launch.

Result: 10+ minutes to first sync, extensive guidance, no friction-free adoption.

## Solution

Redesign onboarding to enable zero-to-sync in under two minutes. Same four pillars as the original, but decomposed so that implementation parallelizes across agents in waves.

1. **In-UI Directory Selection.** Interactive browsing in TUI (Ratatui tree), Web (server-side explorer + autocomplete), GUI (native OS dialog via `rfd`).
2. **Centralized Device Daemon.** One device-level daemon at `$FERRY_HOME/daemon.sock` owning multiple `SyncEngine` instances via `$FERRY_HOME/folders.toml`.
3. **Zero-File In-Band Pairing.** Short 6-word / 6-char rendezvous code negotiated over mDNS or Iroh relay QUIC, handshake entirely in-band.
4. **Self-Bootstrapping Frontends.** Frontends auto-spawn the daemon if the socket is absent.

## User Stories

Same 15 stories as the original, renumbered against waves:

1. As a first-time developer I run a single command (`ferry`) and get a UI without configuring daemons or ports.
2. As a TUI user I press `A` or `O` to open a filesystem browser.
3. As a TUI user I navigate with arrows, `Enter` to descend, `..` to ascend, typing to filter, `Space` to select.
4. As a Web user I click `+ Add Folder` and explore directories with breadcrumbs and quick presets (Home, Projects, Desktop).
5. As a Web user I get live path validation and autocomplete as I type.
6. As a GUI user I click `Select Folder` and get the native OS dialog.
7. As a sharer I get a short 6-word pairing code, no files.
8. As a joiner I enter the code and pick a local destination, devices pair over QUIC automatically.
9. As a LAN/Tailscale user, devices discover each other via mDNS without IP lookup.
10. As a multi-project developer I switch active folder context inside one UI without restarting.
11. As a secret-conscious developer Ferry secret-scans before sharing and surfaces `.env` warnings in the picker.
12. As an auditor I see live per-folder sync progress, peer connectivity, chunk counts in the UI.
13. As a conflicted writer I see quarantined files highlighted with diff/resolve affordance.
14. As an agent wrangler I toggle session pinning per folder in the UI.
15. As a closer I quit the UI and background sync continues undisturbed.

## Domain Model

These types are the Wave 0 deliverable. All later waves depend on them, no wave invents ad-hoc shapes.

```rust
// ferry-ipc/src/fs.rs
pub struct DirectoryEntry {
    pub name: String,          // basename, not full path
    pub path: PathBuf,         // absolute, normalized, NFC
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_git_repo: bool,     // contains .git
    pub is_already_synced: bool, // path is ancestor/descendant of a registered folder
}

pub struct ListDirectoryRequest { pub path: Option<PathBuf> } // None = FERRY_HOME default
pub struct ListDirectoryResponse { pub entries: Vec<DirectoryEntry>, pub absolute_path: PathBuf }

// ferry-ipc/src/registry.rs
pub struct FolderRecord {
    pub folder_id: String,     // hex 32
    pub path: PathBuf,         // absolute
    pub added_at: String,      // RFC3339
}
pub struct FolderRegistry { pub folders: Vec<FolderRecord> } // serialized as folders.toml

// ferry-crypto/src/pairing_code.rs
pub struct PairingCode(String); // 6-word BIP39-like or 6-char base32, constant-time eq
impl PairingCode { pub fn generate<R: Rng>(rng: &mut R) -> Self; pub fn as_str(&self) -> &str; }

// ferry-ipc/src/pairing.rs
pub struct CreatePairingRequest { pub folder_id: String }
pub struct CreatePairingResponse { pub code: String, pub expires_at: String }
pub struct JoinPairingRequest { pub code: String, pub target_dir: PathBuf }
```

Error codes are part of the domain: `bad-path`, `not-a-directory`, `permission-denied`, `path-traversal`, `already-synced`, `not-found`, `pairing-expired`, `pairing-not-found`, `secrets-found`.

## Seams and Core Contracts

Primary seam remains `UiBackend` in `crates/ferry-ipc/src/backend.rs`. Wave 0 extends it with these methods only as signatures. No logic:

```rust
pub trait UiBackend: Send + Sync + 'static {
    // existing 10 methods unchanged
    fn get_status(&self) -> BoxFuture<'_, Result<EngineSnapshot, OpError>>;
    // ... existing share/pin/conflicts/scan/events ...

    // new, added in Wave 0 as trait signatures with default unimplemented!()
    fn list_directory(&self, path: Option<PathBuf>) -> BoxFuture<'_, Result<ListDirectoryResponse, OpError>>;
    fn list_folders(&self) -> BoxFuture<'_, Result<Vec<FolderRecord>, OpError>>;
    fn register_folder(&self, path: PathBuf) -> BoxFuture<'_, Result<FolderRecord, OpError>>;
    fn remove_folder(&self, folder_id: String) -> BoxFuture<'_, Result<(), OpError>>;
    fn create_pairing_session(&self, req: CreatePairingRequest) -> BoxFuture<'_, Result<CreatePairingResponse, OpError>>;
    fn join_pairing_session(&self, req: JoinPairingRequest) -> BoxFuture<'_, Result<PairResult, OpError>>;
}
```

IPC transport adds typed `ClientCommand` / `DaemonMessage` variants for these six methods. No handler logic in Wave 0.

## Wave Plan

Waves are the unit of parallelism. A wave's tickets have no edges between them. They all depend only on earlier waves. Agents in the same wave can run concurrently without merge conflicts because each ticket owns distinct files (see File Ownership).

```
Wave 0 (1 ticket, ~1 day):  01 contracts
        |
Wave 1 (3 tickets, parallel): 02 filesystem seam ─┐
                              03 registry persist ─┼─► all parallel, depend only on 01
                              04 pairing crypto  ─┘
        |
Wave 2 (3 tickets, parallel): 05 TUI picker (dep 02) ─┐
                              06 Web picker (dep 02) ─┼─► parallel, disjoint files
                              07 daemon supervisor (dep 03) ─┘
        |
Wave 3 (2 tickets, parallel): 08 pairing transport (dep 04+07) ─┐
                              09 bootstrap + e2e (dep 07)      ─┼─► parallel
```

Critical path is 01 → 03 → 07 → 08 (4 hops). Wall-clock with 3 agents is ~4 waves, not 9 serial tickets. The original spec's critical path was 01 → 03 → 04 → 05 (4 hops serial plus two more). This plan cuts it to the same length but with 3× parallelism in the middle.

### Wave 0 — Foundation

**01 — Filesystem & Registry Contracts.** Define the types above, add trait signatures, add IPC command variants, add serialization tests. No filesystem reads, no crypto, no daemon code. Unblocks Waves 1 and 2. Files: `crates/ferry-ipc/src/fs.rs` (new), `crates/ferry-ipc/src/registry.rs` (new), `crates/ferry-ipc/src/pairing.rs` (new), `crates/ferry-ipc/src/backend.rs` (trait only), `crates/ferry-ipc/src/protocol.rs` (command variants).

### Wave 1 — Independent Seams (3 parallel)

**02 — Filesystem Backend Seam.** Implement `list_directory` in `FakeBackend` and `InProcessAdapter` (real `std::fs::read_dir` with symlink, git, already-synced detection). Path traversal guard and `FERRY_HOME` scoping. Security must be in this ticket, not later. IPC framing for the command (serialize/deserialize). Unit tests via `UiBackend` contract.

**03 — Folder Registry Persistence.** Implement `FolderRegistry` load/save at `$FERRY_HOME/folders.toml`. Uses `ferry-home` resolution, atomic write via temp-file + rename, file locking, validation (absolute, exists, not nested inside another registered folder). No daemon, no IPC. Pure storage crate. Tested with `tempfile` fixtures.

**04 — Short Pairing Code Crypto.** Implement `PairingCode` in `ferry-crypto`. 128-bit entropy → 6-word wordlist (or 6-char base32) with checksum, constant-time verification, expiry. No network, no folder logic. Tested with vectors and brute-force collision bound.

### Wave 2 — Frontends and Supervisor (3 parallel)

**05 — TUI Filesystem Explorer.** Ratatui modal widget: tree rendering with icons, `..` parent, live filter, arrow/`Enter`/`Space` handling, breadcrumb. Wired to `UiBackend::list_directory`. Tested via `FakeBackend` keyboard simulation, not pixel assertions.

**06 — Web Picker + /api/fs/ls.** Axum `GET /api/fs/ls?path=` with traversal guard (reuse 02's validation), plus JS modal with quick presets, breadcrumb, inline expansion, autocomplete, secret-scan warning display. Tested via `DashboardServer` HTTP integration tests.

**07 — Centralized Device Daemon Supervisor.** Device-level daemon at `$FERRY_HOME/daemon.sock` (Unix) / named pipe (Windows). Reads `folders.toml`, spawns one `SyncEngine` per record, supervises restarts, multiplexes IPC commands (`list_folders`, `register_folder`, `remove_folder`) to the correct engine. Frontends gain `DaemonIpcAdapter` support for the new methods. Tested with two engines under one daemon.

### Wave 3 — Integration (2 parallel)

**08 — Zero-File In-Band Pairing Transport.** Wiring: `create_pairing_session` generates code (04) and advertises via mDNS/Iroh relay using code as rendezvous topic. `join_pairing_session` dials by code, establishes QUIC stream, runs three-way handshake (Offer→Response→Grant) over the stream, persists wrapped FMK into `CONFIG_HEAD`. End-to-end test with two in-memory devices.

**09 — Auto-Bootstrap + Two-Minute E2E.** Frontend auto-spawn: if `daemon.sock` missing, spawn daemon in background, wait for socket, then connect. CLI shortcuts `ferry share <folder>` / `ferry join <code> [dest]`. New `ferry` zero-arg launches the default frontend (GUI→Web→TUI fallback). Acceptance script `scripts/zero-config-e2e.sh` validates two-command share/join across two `$FERRY_HOME` dirs.

## File Ownership (enforced to keep waves parallel)

| Ticket | Owned paths (exclusive) | Shared read-only |
|--------|------------------------|------------------|
| 01 | `crates/ferry-ipc/src/fs.rs`, `registry.rs`, `pairing.rs`, `backend.rs:1-60` (types), `protocol.rs` (variants) | — |
| 02 | `crates/ferry-ipc/src/backend.rs:Fake/InProcess list_directory impl`, `crates/ferry-daemon/src/ui/backend.rs` (list_directory), `crates/ferry-daemon/tests/fs_tests.rs` (new) | `crates/ferry-ipc/src/fs.rs` (read) |
| 03 | `crates/ferry-daemon/src/registry.rs` (new) or `crates/ferry-folder/src/registry.rs`, `crates/ferry-cli/src/home.rs` (read), tests `crates/ferry-folder/tests/registry.rs` | `crates/ferry-ipc/src/registry.rs` (read) |
| 04 | `crates/ferry-crypto/src/pairing_code.rs` (new), `crates/ferry-crypto/src/pairing.rs` (code part only) | — |
| 05 | `crates/ferry-tui/src/picker.rs` (new), `crates/ferry-tui/src/app.rs` (picker wiring only), `crates/ferry-tui/tests/picker_tests.rs` | `crates/ferry-ipc/src/backend.rs` (trait) |
| 06 | `crates/ferry-daemon/src/ui/server.rs` (`/api/fs/ls` only), `crates/ferry-daemon/assets/app.js` (picker modal), `crates/ferry-daemon/tests/server_tests.rs` (fs cases) | `crates/ferry-ipc/src/fs.rs` |
| 07 | `crates/ferry-daemon/src/supervisor.rs` (new), `crates/ferry-daemon/src/ipc/mod.rs` (multi-engine), `crates/ferry-daemon/src/main.rs` (central socket) | `crates/ferry-ipc/src/registry.rs` |
| 08 | `crates/ferry-sync/src/pairing_transport.rs` (new), `crates/ferry-iroh/src/rendezvous.rs` (new), `crates/ferry-crypto/src/folder_key.rs` (handshake) | `crates/ferry-crypto/src/pairing_code.rs` |
| 09 | `crates/ferry-cli/src/bootstrap.rs` (new), `crates/ferry-cli/src/commands/ui.rs` (auto-spawn), `crates/ferry-cli/src/commands/share.rs` + `join.rs`, `scripts/zero-config-e2e.sh` | `crates/ferry-daemon/src/supervisor.rs` |

No two tickets in the same wave write the same file. Cross-wave conflicts are resolved by rebasing onto the earlier wave's merge commit before starting the next wave.

## Testing Decisions

**Seam testing.** All waves test through `UiBackend` and IPC message round-trips. Use `FakeBackend`, `InProcessAdapter`, `DaemonIpcAdapter` uniformly. Do not assert on TUI cell colors or CSS.

**Per-wave verification.**

| Wave | Crate | Test scope |
|------|-------|------------|
| 0 | `ferry-ipc` | Serialization round-trips for new commands, error code table, `FakeBackend` stub returns `not-implemented` |
| 1 | `ferry-ipc`, `ferry-folder`, `ferry-crypto` | FS traversal with symlink/git/already-synced, path traversal exploit attempts, registry atomicity under concurrent writes, pairing code entropy and checksum |
| 2 | `ferry-tui`, `ferry-daemon` | Keyboard navigation against `FakeBackend`, HTTP `/api/fs/ls` auth + traversal, supervisor spawns 2 engines and routes commands |
| 3 | `ferry-sync`, `ferry-iroh`, `ferry-cli` | Two-device in-band pairing over loopback/mDNS, auto-spawn from missing socket, `scripts/zero-config-e2e.sh` green |

**Prior art harnesses.** Extend `crates/ferry-ipc/tests/`, reuse `scripts/quickstart-e2e.sh` / `skeleton-e2e.sh` patterns for the new `zero-config-e2e.sh`.

## Implementation Constraints

- Existing single-folder commands (`ferry init`, `ferry status`, `ferry daemon --listen`) remain functional for CI and headless use. They become thin wrappers over the registry.
- Headless fallback: when `stdout` is not a TTY or `FERRY_HOME` is unset, pickers fall back to explicit path args, no modal.
- Security: path traversal guard is Wave 1 (ticket 02) and reused by Wave 2 (ticket 06). No ticket may expose raw `PathBuf` without validation.
- `ferry-gui` native dialog via `rfd` is optional. Ticket 06's web picker and 05's TUI picker must work without it. Add `rfd` behind `gui` feature, async via `tokio::task::spawn_blocking`.

## Out of Scope

- Cloud accounts, central identity servers (local-first only).
- Mobile clients, VFS/FUSE, partial checkout.
- Multi-user team permissions beyond single-user multi-device.

## Risks and Mitigations

- **Registry corruption.** Mitigate with atomic writes + `folders.toml.lock` + on-load validation with loud error, never silent skip.
- **Central daemon as single point of failure.** Supervisor restarts individual `SyncEngine` on panic, daemon itself is restartable from frontends (Wave 3 bootstrap).
- **mDNS flakiness in CI.** Pairing transport tests use loopback rendezvous first, mDNS second with timeout and skip annotation.
- **File ownership drift.** CI checks that no PR in a wave touches files outside its ownership table.

## Acceptance Criteria for the Feature

- A stranger on macOS and Linux can go from `cargo install` to synced folder in under two minutes via `ferry` → picker → `Share` → 6-word code → `Join`.
- `cargo test --workspace` green, `cargo clippy --workspace --all-targets -- -D warnings` green.
- `scripts/zero-config-e2e.sh` passes with two isolated `$FERRY_HOME` directories on loopback.
