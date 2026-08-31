# Spec: Ponytail Launch Cleanup — Shallow-Module Consolidation & Accidental-Complexity Subtraction

Status: ready-for-agent
Feature Slug: `ponytail-launch-cleanup`
Date: 2026-08-31
Source: ` .scratch/ponytail-audit-2026-08-31.md` (branch `feat/deep-sync-consolidation` d12b524, 78 700 LOC, 223 Rust files, 690 cargo packages) + `get_architecture(all)` + three swarm partitions

## Problem Statement

Ferry is functionally ready but structurally overweight for launch. A whole-repo ponytail audit found ~3 800–4 500 lines of deletable or shrinkable code (~5% of the repo) plus one redundant crate. From the user's perspective this shows up as a confusing install and operation story. From the maintainer's perspective it shows up as three or more places to change one behavior, two sources of truth for one decision, and deep call chains that make a 30-second question take five minutes.

1. **Shallow modules hide complexity instead of removing it.** The sync, backend, and transport seams each carry a facade or duplication that adds reader load without compression. Users see duplicated help text and inconsistent error surfaces. Maintainers keep three implementations in sync by hand.
2. **Two sources of truth for one domain decision.** Routing, rendezvous, sync state, and hex formatting each have two or three registries under different names. Two devices can observe different truth for the same folder, device, or peer, which contradicts the glossary's single-owner rule and ADR-0001/0003/0004.
3. **Speculative and hand-rolled helpers that the platform already ships.** Bitwise CRC-32, custom base32, Gregorian calendar, percent-decoding, LRU, and cursor helpers duplicate stdlib or crates already in the tree. Users see no benefit. Maintainers own bit math that already has a faster, table-driven replacement.
4. **Deep call chains and one-caller wrappers.** Six hot paths require tracing 5–9 layers to answer "where does this manifest decision come from?" Each layer repeats the same arguments, which is append-only complexity per the laziness protocol.
5. **No single place to verify launch readiness.** Findings are scattered across P1 blockers, P2 polish, and P3 cleanup. Launch needs one subtraction wave with one seam per domain and one checklist.

## Solution

Perform one verifiable subtraction wave that deletes shallow modules, collapses dual registries to a single source, and replaces hand-rolled helpers with stdlib or platform features. No new user-facing feature is added. Store format, manifest, blob, and encryption behavior stay identical (ADR-0001, ADR-0002, ADR-0005).

1. **Consolidate shallow modules in the P1 pass (deep-module discipline).**
   - Collapse the `ferry-pin` facade crate into `ferry-sync-engine::pin` so session pinning is a convergence policy, not a separate domain.
   - Collapse the `ferry-daemon::ui::backend` triplication (`AutoBackend`, `DirectBackend`, `InProcessAdapter` across 1 244 lines) onto one parameterized `FolderBackend<StateSource>`.
   - Delete the global `GLOBAL_DIRECTORY` routing table in `ferry-iroh` and pass a single `RouteTable` through `IrohConfig`, removing the split-brain between instance `Inner.routes` and the process-global table.
2. **Collapse remaining dual registries and duplicated choices to one source.**
   - One pairing rendezvous transport (mDNS/relay via iroh or filesystem rendezvous, not both).
   - One sync state enum for all frontends (`SyncState` vs `BeaconState`).
   - One hex formatting seam (`ferry-store` canonical).
   - One cipher seam (`ferry-proto::secure::SessionCipher`).
3. **Replace hand-rolled helpers with stdlib or native crates already in the tree.**
   - CRC-32 via `crc32fast`, base32 via `data-encoding` or a documented short-code alphabet, time via `chrono`/`time`, percent-decoding via `percent-encoding`/`url` (already via `axum`), cursor/put helpers via `std::io::Cursor` + `hex`.
4. **Shrink and flatten without adding layers.**
   - Inline one-caller wrappers (`hold`, `PathMatcher`, `handle_key`, `IrohConfigBuilder`).
   - Replace manual LRU and `StagingPools` branching with a real LRU.
   - Flatten six deep chains to at most three hops per domain question.

Users notice one way to pair, one backend, one cipher, one help entry per command, and faster idle behavior. Maintainers notice one file to change per behavior, one test seam per domain, and a smaller crate graph.

## Seams

Existing seams are preferred to new ones. The highest seam possible is used. Fewer seams is better. The ideal is one seam per domain.

| Domain | Preferred seam (existing) | What is tested through it | Why it is the highest |
|--------|---------------------------|---------------------------|----------------------|
| Convergence and session pinning | `ferry-sync-engine::ConvergenceEngine::converge` + `ferry-sync-engine::pin::PinManager` (after consolidation) | Three-way reconciliation, hold gating, `HeldLedger` materialization and release, `Session pinning` lifecycle | This is the user-visible outcome (which tree lands) and is already exercised by `ferry-sync-engine` matrix tests. Adding a new seam below it would expose internals. |
| UI backend | `ferry-daemon::ui::backend::UiBackend` trait / `ferry-ipc::backend::AutoBackend` or successor `FolderBackend` | Status, pair/share, folder registration, picker `not-initialized` guard, `UiEvent` streaming | One trait serves `ferry-tui`, `ferry-gui`, and `ferry-cli ui`. Testing below it would multiply frontends. |
| Transport and routing | `ferry-sync::transport::Transport` / `ferry-iroh::transport::IrohTransport` + `ferry-iroh::config::IrohConfig` | Dial, listen, route resolution, relay fallback, pairing `PairingRitual` rendezvous | This is the network boundary per `principle-boundary-discipline`. `IrohConfig` construction is where routing truth is injected. |
| Scan and ignore policy | `ferry-scan::engine::ScanEngine` with `ferry-ignore::policy::IgnorePolicy` | Watcher-driven manifest updates, ignore rule enforcement, incremental scan | `ScanEngine` is already the only manifest source after `deep-sync-consolidation`; testing through `snapshot_dir_incremental` directly would bypass the policy seam. |
| Folder lifecycle | `ferry-folder::inventory` + `ferry-folder::pairing::PairingRitual` | Folder bootstrap, `is_initialized` guard, pairing code mint/answer, `CONFIG_HEAD` creation | This is where the glossary's Folder and Pairing terms are enforced before any store or sync code runs. |

New seams proposed: none. The single proposed structural seam is the crate-boundary move of `ferry-pin` into `ferry-sync-engine::pin`. It reuses the existing `ConvergenceEngine` and `HeldLedger` seam and does not add a new test seam.

Check: these seams match the `deep-sync-consolidation` and `surface-pruning` seams already in use (`SyncEngine` exchange seam, `AutoBackend` seam, `PickerState::try_select` seam). Callers are migrated before the old symbol is deleted in one wave per `migrate-callers-then-delete-legacy-apis`.

## User Stories

1. As a new user running `cargo install` or the install script, I want one fewer crate in the workspace graph, so that compile times and docs list one less concept to learn.
2. As a new user reading `README` or `cargo doc`, I want session pinning described as part of convergence, so that I do not wonder which crate owns "held edits."
3. As a maintainer adding a pin policy change, I want one module `ferry-sync-engine::pin` to edit, so that I do not chase re-exports across two crates.
4. As a maintainer reviewing a pin fix, I want one `HeldLedger` and one `PinManager` type, so that there is one place to audit ledger persistence.
5. As a multi-device user with a folder pinned on device A, I want the convergence engine to hold competing edits transactionally, so that my agent on A never sees torn writes.
6. As a device operator releasing a pin, I want one `release` call that converges and clears the ledger atomically, so that held edits do not vanish on restart.
7. As a dashboard user opening the daemon UI, I want `get_status` and `share` to behave identically whether the daemon is local or remote, so that I trust one product.
8. As a TUI user, I want the same status badges and peer list as the GUI, so that documentation screenshots match my terminal.
9. As a GUI user, I want the daemon connection to fall back to local disk transparently, so that I can manage folders offline.
10. As a maintainer adding a new backend endpoint, I want to edit one `FolderBackend` implementation, so that I do not duplicate `is_transport` fallback arms in three places.
11. As a maintainer fixing a backend bug, I want one set of `is_transport` fallback logic, so that the fix ships to every frontend at once.
12. As a daemon operator, I want the backend dispatch macro or helper to enforce uniform transport error handling, so that 500s are not frontend-specific.
13. As an operator adding a route for a folder, I want that route stored in one `RouteTable` injected via `IrohConfig`, so that I never see stale global state.
14. As a developer debugging a dial failure, I want one place to inspect route resolution, so that I do not check both an instance map and a global `OnceLock`.
15. As a relay operator, I want the iroh transport to own one routing truth, so that relay fallback and direct QUIC agree.
16. As a pairing user typing a 6-char code `234567` (ADR-0006), I want pairing to use one rendezvous transport (mDNS/relay), so that I never see a file-system fallback contradict a relay success.
17. As a pairing user on a single machine testing with two folders, I want the rendezvous decision documented if the filesystem file is kept for cross-process testing, so that I understand which path was taken.
18. As a TUI user pressing `Esc`, `q`, or `Enter` in the picker, I want one keymap implementation, so that `handle_key` and `handle_key_action` never diverge.
19. As a TUI maintainer changing a shortcut, I want to edit one `match key.code` tree, so that help text and behavior stay consistent.
20. As a crypto reviewer auditing session setup, I want one `SessionCipher` implementation, so that a fix in `ferry-proto` does not miss a copy in `ferry-sync::session::DirectionCipher`.
21. As a security auditor, I want the wire cipher boundary at `ferry-proto::secure`, so that handshake and application data share the same KDF and nonce discipline (ADR-0002).
22. As a user sharing a pairing QR, I want short-code generation to use one base32 alphabet, so that typeability and checksum behavior are uniform (ADR-0006).
23. As a user generating a pairing code, I want CRC-32 verification via the standard table-driven implementation, so that single-character typo detection is not hand-rolled bit math.
24. As a maintainer reading pairing code, I want the base32 alphabet decision explicitly owned (custom 20-symbol short code vs RFC4648), so that a future contributor does not reintroduce two alphabets.
25. As a developer editing `ferry-sync-engine` hold logic, I want `hold` helpers inlined into `ConvergenceEngine`, so that I do not jump between `hold.rs`, `matcher.rs`, and `converge.rs` for one gating decision.
26. As a developer adding an ignore pattern, I want `IgnorePolicy` enforced once at the scan seam, so that held and converged trees agree on what is ignored.
27. As a code reviewer reading converge, I want `reconcile` to return `chunks_held` directly, so that `gate_plan` does not need a second DFS over the tree (`chunk_path_map`).
28. As a consumer of `ferry-platform`, I want `format_bytes` in one place, so that TUI and GUI render `1.2 MB` identically.
29. As a consumer of `ferry-store`, I want `hex`/`short_hex` in one place, so that device and manifest IDs truncate to the same length everywhere.
30. As a GUI theme designer, I want one sync state enum, so that `SyncState::Pinned` and `BeaconState::Holding` do not diverge on name, color, or pulse speed.
31. As a TUI render author, I want one badge table for `Syncing`/`Conflict`/`Pinned`/`Idle`, so that adding a state does not require editing three renderers.
32. As a web dashboard consumer, I want `api/fs/ls` to trust the single `ferry-folder::inventory::validate_path` guard, so that path validation is not duplicated between `axum` and `inventory`.
33. As a web operator, I want query param decoding via the standard `percent-encoding` crate already pulled by `axum`, so that `%2e`/`%00` handling is not hand-rolled.
34. As a store maintainer tweaking pack eviction, I want `PackCache` backed by a real `LruCache`, so that `order.remove(pos)` is not an O(n) scan on every `get`.
35. As a store maintainer staging blobs, I want `StagingPools::offer` to branch once on `is_meta`, so that `data` and `meta` pools share the same logic.
36. As a developer reading pack code, I want one `pool(is_meta)` helper, so that pool capacity and byte math are not duplicated.
37. As a reader of `ferry-store::format`, I want `put_u32`/`hex`/`Reader` replaced by `extend_from_slice(&v.to_le_bytes())` / `hex` crate / `std::io::Cursor`, so that I do not learn a private mini-encoding.
38. As a contributor adding a new wire field, I want `Reader::take_array` style helpers to exist in one place, so that `pairing.rs` and `config_head.rs` do not each own a `Reader` struct.
39. As a platform maintainer, I want Gregorian calendar math via `chrono` or `time`, so that `civil_from_days` 80-line hand-roll is not owned.
40. As a scan maintainer, I want `mtime_sec`/`split_unix`/`live_exec` owned once in `ferry-platform`, so that `ferry-scan` and `ferry-materialize` do not duplicate `PermissionsExt::mode & 0o111`.
41. As a transport maintainer, I want `IrohConfigBuilder` replaced by struct literal `IrohConfig { secret: Some(...), ..Default::default() }`, so that a factory with one product is not kept as a fluent API.
42. As a folder bootstrap caller, I want `create_folder` and `adopt_folder` to share `init_store`, so that `Store::create` + polynomial + key wrapping is not 90% duplicated.
43. As a folder inventory caller, I want `ferry_home` and `default_listing_root` to share env-var precedence `FERRY_HOME → current_dir/HOME → /tmp`, so that the listing root and the home root cannot diverge.
44. As a CLI author adding a new subcommand, I want no thin `ferry-cli::folder` re-export layer that only maps `map_err(CliError::from)`, so that I call `ferry-folder` directly.
45. As a developer reading `ferry-store::store`, I want `REBUILD_INDEX_CALLS` and `DEBUG_LOCKS` gated or removed, so that `FERRY_DEBUG` probe counters are not shipped metrics without a consumer.
46. As a test author writing an IPC test, I want `FakeBackend` in `crates/ferry-ipc/tests/` or behind `#[cfg(test)]`, so that `crates/ferry-ipc/src` contains only the shipped `UiBackend` contract.
47. As a convergence maintainer, I want one conflict landing path (exclusive `write_loser_copy` with counter loop), so that `naming::unique_conflict_dest` advisory probe does not duplicate collision logic.
48. As a Windows user, I want `\\?\` prefix handling clearly gated to Windows, so that POSIX builds do not compile 210 lines of identity `extend_path`.
49. As a materialization maintainer, I want `validate_components` colon guard gated to Windows, so that POSIX paths are not rejected for containing `:` unnecessarily.
50. As a TUI author optimizing draw latency, I want render strings computed in `render_*` or memoized once, so that `update_cached_strings` is not called from seven `apply_*` methods to save one `format!` per 200 ms.
51. As a dashboard user requesting `GET /` or `/index.html` or `/app.js`, I want one `asset` + fallback handler, so that the router does not register five paths to the same index.
52. As a convergence author, I want `PinError` collapsed into `ConvergenceError` or `thiserror` upstream, so that `StructuralSplit` message strings are not duplicated across two enums.
53. As a sync engine reviewer, I want the BFS guard to be one `BUDGET` instead of three (`MAX_BFS_ROUNDS`, `MAX_ADVERT_ROWS_TOTAL`, `MAX_BATCHES_PER_ROUND`), so that pull budgeting is auditably one decision.
54. As a release manager cutting a launch tag, I want the crate count and `cargo tree` depth unchanged except for the `ferry-pin` deletion, so that `lean` feature stripping is the only size optimization to document.
55. As a launch readiness reviewer, I want the deep call chain questions ("where does this manifest decision come from?") answerable in under three hops, so that onboarding a new maintainer does not require tracing seven files.

## Implementation Decisions

- **Decision: `ferry-pin` facade collapsed into `ferry-sync-engine::pin`.** The workspace crate count drops by one. The module `ferry-sync-engine::pin::{manager,release,ledger,matcher}` becomes the deep module for session pinning per `model-the-domain` and `subtract-before-you-add`. The existing symbols `PinManager`, `HeldLedger`, `HeldEntry`, `PinRecord`, `PathMatcher`, `hold_matcher`, `record_held` are moved, not re-abstracted. Re-exports remain temporarily behind `ferry-pin` as `pub use ferry_sync_engine::pin::*` for one commit wave, then the crate is removed from the workspace and from the six dependents' `Cargo.toml`. This is `migrate-callers-then-delete-legacy-apis`. Pinning stays a convergence policy, not a separate crate domain. No new trait is introduced.

- **Decision: `ferry-daemon::ui::backend` triplication collapsed to `FolderBackend<StateSource>`.** The three types `AutoBackend`, `DirectBackend`, and `InProcessAdapter` (1 244 lines total, `get_status` 70% identical, `share`/`pair` duplicated three ways differing only by `spawn_blocking`) are replaced by one `FolderBackend<S>` parameterized over a `StateSource` that provides `open_folder`, `list_folder`, `pin_state`, and `event_stream`. The existing `UiBackend` trait stays the boundary per `boundary-discipline`. The `is_transport` fallback arms (10 copy-paste `match client.X().await { Ok=>Ok, Err(e) if e.is_transport()=>fallback }` in `ferry-ipc::backend`) are macro-extracted to one site. `DashboardServer` composes `FolderBackend` directly. `Supervisor` no longer needs to know which backend variant is active.

- **Decision: `ferry-iroh` dual routing table deleted.** The process-global `GLOBAL_DIRECTORY: OnceLock<RouteTable>` is removed. The instance `Inner.routes` injected via `IrohConfig::routes` is the single registry. Dial resolution checks `self.inner.routes.resolve_route` only, with no global fallback per `separate-before-serializing-shared-state`. Route construction moves to `IrohConfig` builder site or struct literal, and `RouteTable` is passed strictly as owned/config state, not via ambient global. This closes the split-brain where two maps store the same `RouteKey→Route`.

- **Decision: one pairing rendezvous transport.** The dual rendezvous (in-memory `SharedRendezvous: Arc<Mutex<HashMap>>` plus filesystem `/tmp/ferry-rendezvous-<CODE>.json` `write/read/remove_rendezvous_file` plus no-op `ferry-iroh::rendezvous::advertise/discover` stubs) is collapsed to one transport: iroh rendezvous via mDNS topic and relay fallback per ADR-0003/ADR-0006. If cross-process single-machine testing requires a seam, the in-memory map is kept as the test seam behind `#[cfg(test)]` or as an explicit `PairingRitual::InMemory` constructor, not as a second production path checked alongside the filesystem. `peek_session` no longer checks two stores.

- **Decision: one cipher seam.** `ferry-sync::session::DirectionCipher` (115 lines, comment "Byte-compatible with `ferry_proto::secure::SessionCipher`") is deleted. `ferry-proto::secure::SessionCipher` and its KDF are the single implementation. Session establishment in `ferry-sync` calls the proto cipher directly. No construction is duplicated.

- **Decision: one scan seam with authoritative ignore binding.** The sync orchestration seam stays `SyncEngine` + `ScanEngine`. Raw `snapshot_dir_incremental` calls in sync loops are forbidden. Every local manifest update goes through `ScanEngine` bound to the folder's compiled `FerryIgnore` via `Arc<dyn IgnorePolicy>`. `PinGate` trait in `ferry-materialize::apply` is deleted as already decided in `deep-sync-consolidation`.

- **Decision: stdlib and native replacements already in the tree.** CRC-32 via `crc32fast::hash`, base32 via `data-encoding` (or retain the custom 20-symbol short-code alphabet with a comment explaining the RFC4648 divergence), percent-decoding via `percent-encoding`/`url::form_urlencoded` (already via `axum`), cursor and hex via `std::io::Cursor` + `hex` crate (already via `blake3`), Gregorian time via `chrono`/`time`, LRU via `lru::LruCache`. No new heavy dependency is introduced. `tokio 1.49.0` and `iroh 1.0.3` pins remain per ADR-0003.

- **Decision: one-caller wrappers inlined, one-product factories removed.** `ferry-sync-engine::hold::hold_matcher`/`record_held` and `ferry-sync-engine::matcher::PathMatcher` (wrapper over `ignore::gitignore::Gitignore`) are inlined into `ConvergenceEngine`. `IrohConfigBuilder` fluent setters are replaced by struct literals. `TuiState::handle_key` vs `handle_key_action` duplicated `match key.code` trees are collapsed to one async handler with a sync wrapper. `ferry-cli::folder` thin re-export layer and `ferry-daemon::folder_engine` 5-line re-export are deleted.

- **Decision: deep chains flattened to at most three hops per question.** `ConvergenceEngine::reconcile` returns `chunks_held` so `gate_plan` needs no second DFS `chunk_path_map`. BFS guards collapse to one `BUDGET`. `IrohTransport::dial` requires `tokio::runtime::Handle` injection instead of owning `Mutex<Option<Runtime>>` per instance. `ScanEngine::watch_with` collapses `PolicyState` and `DirCache` indirection. `Supervisor::spawn_engine` + `FolderEngine::start_internal` collapse to one `SyncEngine::open_watched_folder(path)` per folder. Each flattening is ordered as subtract before add.

- **Decision: validators unified at boundaries.** `ferry-folder::inventory::validate_path` is the single path-traversal guard. `ferry-daemon::ui::server::percent_decode_query_value` and `api_fs_ls` pre-checks are deleted. `ferry-platform::winpath::extend_path` and `ferry-materialize::apply::validate_components` colon guard are gated `#[cfg(windows)]`. `NAME_LEN_LIMIT` hashing is documented or removed. Boundaries validate, internals trust types per `boundary-discipline`.

- **Decision: no schema or wire-format change.** Pack, blob, manifest, and `CONFIG_HEAD` formats are unchanged. `PairingCode` format (6-char base32, CRC-32 checksum, 24 h expiry, constant-time verify) is unchanged per ADR-0006. ADR-0002 encryption, ADR-0004 conflict quarantine (`*.ferry-conflict.*` + structured report), and ADR-0007 refuse-by-default peer policy remain. ADR-0003 QUIC + relay transport stays behind the `Transport` trait.

- **Decision: crate and module ownership after consolidation.** `ferry-store` remains the deep module for blobs, packs, manifests, and format. `ferry-sync-engine` remains the deep module for convergence, reconciliation, and pinning. `ferry-iroh` remains the deep module for transport. `ferry-platform` remains the deep module for `casefold`, `links`, `lock`, `procs`, `reserved`, `winpath`, `time`. `ferry-scan` remains the deep module for walking and watching. `ferry-folder` remains the deep module for Folder lifecycle and `PairingRitual`. `ferry-ipc` remains the deep module for IPC framing and the universal backend seam. `ferry-daemon` remains the thin supervisor and dashboard shell.

## Testing Decisions

A good test asserts external observable behavior through the highest seam that can observe it, never internal helper call counts, branch coverage, or private struct fields. One seam for one behavior. New seams are not added when an existing higher seam already observes the outcome. The fewest seams possible is the goal. The ideal is one.

- **Seam for convergence and session pinning:** existing `ferry-sync-engine` matrix and convergence tests plus `ferry-pin` ledger tests re-routed through `ferry-sync-engine::pin` after the crate move. Tests assert that while a folder is pinned, competing remote edits are held in `HeldLedger` and survive daemon restart, and that `release` converges and clears the ledger transactionally. Prior art is `crates/ferry-sync-engine/tests/matrix.rs`, `crates/ferry-pin/tests/`, and `crates/ferry-sync/tests/reconciliation_quarantine.rs`.

- **Seam for UI backend:** existing `ferry-ipc` contract tests and `ferry-daemon` backend tests rewritten against `FolderBackend<StateSource>`. Tests assert that `get_status`, `share`, `pair`, and `register` produce identical results via local and daemon paths, and that `is_transport` fallback is uniform. Prior art is `crates/ferry-ipc/tests/contract_tests.rs`, `crates/ferry-daemon/tests/backend_tests.rs`, and the `multi_frontend_consistency` assertion that currently exists only to keep `SyncState` vs `BeaconState` in lockstep.

- **Seam for routing and transport:** existing `ferry-iroh` and `ferry-sync` transport tests plus the pairing ritual round-trip tests. Tests assert that a route added via `IrohConfig::routes` is resolved by dial without global fallback, that mDNS and relay fallback agree, and that `PairingRitual` rendezvous succeeds through the single transport. Prior art is `crates/ferry-iroh/tests/`, `crates/ferry-sync/tests/peer_policy.rs`, and `crates/ferry-folder/tests/pairing/` (`ritual.rs`).

- **Seam for scan and ignore policy:** existing `ferry-scan` and `ferry-folder` inventory tests driven through `ScanEngine`. Tests assert that paths matching `FerryIgnore` never appear in peer manifests or on peer disks. Prior art is `crates/ferry-scan/tests/scan_tests.rs`, `crates/ferry-ignore/tests/`, and `crates/ferry-sync/tests/ignore_policy_sync.rs`.

- **Seam for CLI and picker guards:** existing `ferry-cli` `cli_parse` table-driven tests and `ferry-tui` picker tests. Tests assert that an uninitialized directory is refused with `FolderError::not_initialized` via `ferry-folder::is_initialized` across TUI, GUI, and daemon, and that `ferry --help` lists one init entry after `surface-pruning`. Prior art is `crates/ferry-cli/tests/cli_parse.rs`, `crates/ferry-tui/tests/picker_tests.rs`, `crates/ferry-gui/tests/gui_tests.rs`.

- **Negative cases:** unpaired devices with ADR-0007 refuse policy still refuse. `BFS BUDGET` exhaustion still quarantines. `FakeBackend` moved to tests does not affect shipped `UiBackend` contract. `crates/ferry-iroh/src/rendezvous.rs` stubs are absent in production builds.

## Out of Scope

- New transport, relay, or store format changes. Wire bincode framing in `ferry-proto`, pack/blob encryption via ChaCha20-Poly1305/BLAKE3, CDC chunking constants, and `CONFIG_HEAD` layout are unchanged.
- Hosted relay discovery or account system. ADR-0003 self-hostable relay remains.
- Mobile OS support and Windows symlink privilege escalation beyond current non-admin fallback.
- Content-defined chunker tuning or CDC benchmark changes (ADR-0005).
- Re-adding `ferry add` alias, unauthenticated `ferry daemon --ui`, dummy daemon fallback, or duplicate `docs/manual-testing-guide.md`. Those deletions are already closed in `surface-pruning`.
- Adding a new web frontend framework or replacing `axum`/`tokio` versions. Pins stay per T-01.
- Performance hillclimb or scan-throughput benchmarking beyond verifying that idle CPU does not regress after `ScanEngine` consolidation.
- Accessibility or fluid-glass motion changes (separate `fluid-glass-motion`/`fluid-glass-ui` tracks).

## Further Notes

This spec closes the deferred `shallow-module` half flagged as `arch-deepening` tickets 01–06 and the ponytail P1 blockers in `.scratch/ponytail-audit-2026-08-31.md`. It is the subtraction counterpart to `deep-sync-consolidation`, which unified orchestration and convergence into a deep engine. This pass does not deepen further. It removes facades that wrap deep modules without adding leverage.

Ordering is subtract before add, migrate callers then delete, one wave per domain: crate move `ferry-pin` first (workspace graph), then `ferry-iroh` global deletion, then `ferry-daemon::ui::backend` parameterization. Each commit is verifiable with `cargo test -p ferry-sync-engine -p ferry-iroh -p ferry-daemon -p ferry-ipc`, `cargo tree --depth 1`, and `rg` for the deleted symbol returning zero. The HTML build `ferry_architecture_and_ponytail_audit_report.html` captures the visual crate seams for review but is not a source of truth. New tests are not added for deleted helpers (`base32`, `crc32`, `IrohConfigBuilder`, `format_bytes`, `FakeBackend` in `src/`) beyond asserting their replacements produce identical external output through the higher seams.

