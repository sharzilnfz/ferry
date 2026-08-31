# Spec: Surface Pruning and Complexity Subtraction

Status: done

## Problem Statement

Ferry's command and documentation surface still carries v0 weight that was slated for removal in `v0-launch-readiness` ticket 07 but was intentionally deferred during the `feat/deep-sync-consolidation` merge because every Rust file from `v0-readiness-spec` was weaker than `HEAD`. From the user's perspective the surface is noisy. From the maintainer's perspective it is cheap but duplicative.

1. **Duplicate init command.** `ferry add` and `ferry init` do the same Store creation. Two names for one Folder bootstrap doubles help text, docs, and `cli_parse` tables with no user gain. New users hesitate over which to type.
2. **Unauthenticated daemon web flag.** `ferry daemon --ui [HOST:PORT]` serves a loopback dashboard with no auth token. The product has a token-authenticated `ferry ui --web` and `ferry-daemon` DashboardServer. Keeping both invites binding mistakes and doubles `DAEMON_AFTER_HELP`.
3. **Dummy daemon fallback in bootstrap.** `ferry-cli` bootstrap spawns a minimal `ferry_ipc::IpcServer` thread that answers `Ping` with `Pong` when the real daemon binary is not built. Tests stay green without a daemon, but operators never see the real `daemon-start-failed` path and log triage is noisy.
4. **Duplicate manual testing guide.** `docs/manual-testing-guide.md` and `MANUAL_TESTING_GUIDE.md` diverge by 114 lines. Two sources of truth for the Big Picture and dual-device topology. Readers pick the wrong one.
5. **Scattered prune intent.** The prune was ticketed as `v0-launch-readiness` 07 but never landed as a single registry. Future prunes have no place to record what was considered and kept.

All of this sits on top of a merge that already proved `v0` provided no superior Rust logic. The remaining work is purely surface subtraction, not behavior addition. It must happen as migrate-callers-then-delete, not as a blind delete.

## Solution

Prune the CLI and docs surface in one verifiable subtraction, keep the superior `feat` behavior untouched, and record the registry explicitly.

1. **Consolidate init surface.** Keep `ferry init` as the single authoritative Folder creation command. Remove `ferry add`. Existing `add` callers are migrated to `init` in one wave. Help epilog and `cli-json` are updated so there is exactly one init entry.
2. **Remove daemon web flag.** Delete `ferry daemon --ui` and its `DAEMON_AFTER_HELP` stanza. The web dashboard remains exclusively via `ferry ui --web` and `--web` on the appropriate `Ui` command. `dashboard-e2e` and `skeleton-e2e` are pointed at the surviving entry point.
3. **Delete dummy daemon fallback.** Remove `start_dummy_daemon` from the CLI bootstrap seam. Bootstrap failures now surface immediately as `daemon-start-failed` with `check $FERRY_HOME permissions`. Tests that previously relied on the dummy are pointed at the real `FerryStore` seam or an explicit `FakeBackend`.
4. **Eliminate duplicate guide.** Delete `docs/manual-testing-guide.md` and keep `MANUAL_TESTING_GUIDE.md` at the project root as the single source of truth. No content is merged. The root guide is already the longer, topology-complete version.
5. **Force picker and backend through initialization guard.** No change needed. This is already satisfied by `ferry-folder::is_initialized` and `FolderError::not_initialized` consumed by `ferry-tui` `PickerState::try_select`, `ferry-gui` `GuiApp`, and `ferry-daemon` `ui::backend`. The decision is to assert it, not to rebuild it.

The result is a smaller command surface with the same Store, Manifest, and SyncEngine behavior. No new feature is added.

## User Stories

1. As a new developer, I want one init command `ferry init`, so that I do not hesitate between `init` and `add`.
2. As a new developer, I want `ferry --help` to list one init entry, so that I learn the happy path in one glance.
3. As a developer reading `docs/cli-json.md`, I want one init schema, so that code-generated CLIs do not branch on two names.
4. As an operator running `ferry daemon`, I want no `--ui` flag on that command, so that I do not accidentally expose an unauthenticated loopback dashboard.
5. As an operator, I want `ferry daemon --help` to describe only daemon concerns, so that I do not confuse sync transport with web serving.
6. As an operator who typed `ferry daemon --ui`, I want a clear `unknown argument` error, so that I immediately discover `ferry ui --web`.
7. As a developer running `ferry ui --web`, I want that to remain the single authenticated dashboard entry, so that I trust one token path.
8. As a maintainer, I want `cli.rs` to have one init variant, so that I add flags in one place.
9. As a maintainer, I want `main.rs` to have one init dispatch, so that I do not duplicate `init::run` wiring.
10. As a test author, I want no dummy `IpcServer` thread in bootstrap, so that a failing daemon start is visible as `daemon-start-failed` not a silent `Pong`.
11. As a test author, I want bootstrap tests to use the `FerryStore` or `FakeBackend` seam, so that tests do not depend on a hidden in-process server.
12. As an operator debugging a failed start, I want the log to contain the real `bootstrap` error with hint `check $FERRY_HOME permissions`, so that I fix permissions instead of hunting a phantom server.
13. As a developer reading docs, I want one manual testing guide at `MANUAL_TESTING_GUIDE.md`, so that I do not open the stale `docs/` copy.
14. As a docs maintainer, I want one Big Picture and one dual-device topology, so that I update one file and every reference stays correct.
15. As a maintainer, I want the prune registry to live in this spec, so that future prunes have a place to record what was kept and why.
16. As a TUI user picking a Folder, I want the picker to refuse an uninitialized directory with `FolderError::not_initialized`, so that I run `ferry init` or accept a pairing offer before sync.
17. As a GUI user picking a Folder, I want the same `not_initialized` guard, so that behavior is identical across frontends.
18. As a Web UI user, I want the same backend guard via `InProcessAdapter`, so that all three surfaces enforce the same Store bootstrap rule.
19. As a maintainer adding a new surface, I want the initialization guard to remain centralized in `ferry-folder`, so that I do not reimplement the `.ferry` check.
20. As a release manager, I want `scripts/install.sh` and `.github/workflows/release.yml` unaffected by this prune, so that the `06` release packaging stays green.

## Implementation Decisions

- **Decision: single init registry.** The CLI command registry keeps `Init` and deletes `Add`. No alias, no hidden compat. The choice is recorded as one source of truth. All help, shell completions, and JSON schema are regenerated from that registry. This is the migrate-callers-then-delete pattern. Callers are inventoried via `grep` for `Command::Add` and `ferry add`, migrated to `ferry init`, then the old enum variant is deleted in the same commit wave. No shim is left behind.

- **Decision: daemon is transport only.** The daemon command keeps its transport and supervision concerns (`listen`, `peer_url`, `transport`, `interval_secs`, `DaemonAction::{Stop,Status}`) and deletes its web concern. The web dashboard stays in the `Ui` surface with token auth. `DAEMON_AFTER_HELP` describing the unauthenticated loopback dashboard is deleted. `Ui` remains the boundary for browser concerns per Boundary Discipline.

- **Decision: bootstrap fails loudly.** The CLI bootstrap seam stops spawning an in-process `IpcServer` fallback. The seam remains a pure Store and daemon spawn path. Store creation failures and daemon spawn failures propagate as typed errors with codes `daemon-start-failed` and hint `check $FERRY_HOME permissions`. No retry, no silent `Pong`.

- **Decision: docs have one canonical guide.** The docs module keeps `MANUAL_TESTING_GUIDE.md` at the root. The duplicate `docs/manual-testing-guide.md` file is removed. No content merge is attempted. References in `README` and `quickstart` are pointed at the root guide. The file boundary is the docs seam.

- **Decision: picker guard stays centralized.** The Folder initialization guard is already modeled as `ferry-folder::is_initialized` and `FolderError::not_initialized` with remedy hint `run ferry init or ferry pair`. The TUI picker, GUI app, and Web UI backend all consume that single predicate. No new guard is introduced. This is the correct Domain model. Scattered `.join(".ferry").is_dir()` checks remain deleted.

- **Decision: ordering and idempotence.** The prune is ordered as docs removal first, then CLI registry deletion, then bootstrap fallback deletion. Each step is idempotent. Running the prune twice yields the same tree. No intermediate state introduces a second init path.

- **Decision: out-of-scope retention.** The work explicitly keeps `scripts/install.sh`, `.github/workflows/release.yml`, and `docs/test.md` sanitization from ticket `06`. Those are boundary infra, not surface prune.

## Testing Decisions

Good tests assert external observable behavior, never internal branch counts or private helper state. The highest seam is preferred. One seam for one behavior.

- **Seams preferred:** CLI table-driven parsing in `cli_parse`, daemon bootstrap via `try_ping_sync` and `Cargo` Store open, TUI picker via `PickerState::try_select`, GUI app via `GuiApp::handle_event`, docs via file existence check. No new seam is introduced for this prune.
- **CLI surface:** Existing table-driven `cli_parse` tests are updated to assert that `ferry add` is rejected with `unknown subcommand` and `ferry init` still parses. Prior art is the `cli_parse` table that already covers `init`, `pair`, `share`, `join`, `daemon`, `ui`.
- **Daemon surface:** Bootstrap tests assert that a missing daemon binary yields `daemon-start-failed` with the permissions hint, not a silent success. Prior art is `bootstrap_tests` and `daemon_lifecycle` that already assert `DaemonLock` and `TerminateOutcome`.
- **Picker surface:** `picker_tests` and `gui_tests` assert that selecting an uninitialized directory returns `FolderError::not_initialized` with code `not-initialized` and remedy hint, without dispatching `BackendAction::RegisterFolder`. Prior art is `picker_tests` for `NotInitialized` and `gui_tests` for `is_initialized`.
- **Docs surface:** A single file existence assertion proves `docs/manual-testing-guide.md` is absent and `MANUAL_TESTING_GUIDE.md` is present. No content assertion.
- **Negative cases must stay green:** `dashboard-e2e` and `skeleton-e2e` are run against the surviving `ferry ui --web` path. Prior art is `scripts/quickstart-e2e.sh` and `scripts/skeleton-e2e.sh` already invoked in CI.

## Out of Scope

- New transport, relay, or Store format changes.
- Windows symlink privilege escalation beyond current non-admin fallback.
- Hosted relay discovery or account system.
- Mobile OS support.
- Content-defined chunker tuning or CDC benchmark changes.
- Reintroducing a compat alias for `ferry add`. The delete is final.
- Re-adding an unauthenticated web flag elsewhere.

## Further Notes

This spec closes the deferred half of `v0-launch-readiness` 07. The original `07-v0-complexity-subtraction-and-pruning.md` was not cherry-picked from `v0-readiness-spec` because every Rust file on that branch was weaker than `feat/deep-sync-consolidation` (pid-reuse `ProcessLock`, `root_tree_id` lineage check, `DaemonIpcAdapter` duplication, `supervisor.rs` monolith). Commit `a22368c` proved that by grafting only the four surviving artifacts from ticket `06` and keeping `HEAD` for every `crates/` file. This spec records that decision and finishes the remaining deletions as a single subtraction wave with no new behavior.
