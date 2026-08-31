# Specification: Deep-Module Consolidation and Shippability Hardening

Status: ready-for-agent

Supersedes `.scratch/v0-review-remediation/spec.md`. That spec's remediations 1, 3,
and 4 were partially or not implemented in commit badcca5, and remediation 2 was
skipped entirely. This spec folds those requirements in, plus the deepening
opportunities surfaced by the two-axis review and architecture review of the same
commit. Verified facts this spec answers: cargo clippy and the full per-crate test
suite pass, cargo fmt fails in the duplicated daemon lifecycle code, and roughly
700 tests pass with zero failures.

## Problem Statement

Ferry v0 is close to shippable, but its most important policies live in the wrong
places. A developer whose folder key unwrap fails gets a silently plaintext,
zero-key store that syncs anyway. A developer whose daemon hangs during stop loses
the PID file and cannot start a new daemon without archaeology. A developer whose
machine was restored from a snapshot can have locally restored files clobbered,
because reconciliation only verifies lineage on the remote side. TUI and GUI users
can register uninitialized directories straight into sync. And a maintainer
touching daemon lifecycle must edit the same signal handling, PID parsing, and
lock teardown in two crates, hoping both copies stay identical.

## Solution

Every policy decision moves behind the module that owns its domain. ferry-folder
becomes the single Store-opening interface: it derives the folder key, picks the
cipher, and fails loud. ferry-daemon gains one device-daemon entry and
ferry-platform gains one DaemonLock interface, so stop and status are pure
functions of a directory. Reconciliation proves the base is an ancestor of both
local and remote manifests, degrading to an empty base on any broken lineage.
Trust policy lives on PeerPolicy instead of mid-session. TUI and GUI refuse
uninitialized directories through a check ferry-folder owns. What is fake or
unused gets deleted rather than wired twice.

## User Stories

1. As a developer whose local machine was restored from a snapshot, I want reconciliation to verify that the sync base is an ancestor of my local manifest as well as the remote manifest, so that my locally restored files are never silently deleted.
2. As a developer syncing a folder across two devices, I want both devices to derive the chunker polynomial from the store rather than from a hash guess, so that chunks line up after either side rebuilds its binary.
3. As a developer with a stale device key or an unshared folder, I want the daemon to fail loudly instead of reopening my folder as an unencrypted store, so that my source code never touches disk in plaintext.
4. As a security reviewer, I want exactly one module that decides cipher and master-key policy, so that I can audit the encryption invariant in one place.
5. As a developer using the TUI, I want the folder picker to validate that a selected directory is initialized with Ferry before registering it, so that I cannot accidentally register an uninitialized folder into synchronization.
6. As a developer using the desktop GUI, I want the folder picker to prevent registering an uninitialized folder, so that I receive immediate guidance to initialize or pair the project first.
7. As a developer stopping the daemon via `ferry daemon stop`, I want the command to poll with backoff up to a five-second deadline and report an error if the daemon does not exit, so that I know the daemon is still active.
8. As a developer checking `ferry daemon status` after a hung stop attempt, I want status to report the live PID, so that I am not misled by a missing PID file while the lock is held.
9. As an operator running `ferry daemon stop`, I want the PID file to remain intact until the process is verified dead, so that monitoring tooling observes coherent state.
10. As an operator managing Ferry across machines, I want consistent SIGINT and SIGTERM behavior whether launching the daemon directly or through the CLI, so that all cleanup hooks run reliably.
11. As a maintainer, I want PID parsing and liveness checks to live in one canonical interface on the platform module, so that bug fixes to process management apply universally.
12. As a maintainer, I want the device daemon to have one entry point, so that adding lifecycle behavior does not require editing two crates in lockstep.
13. As a maintainer reviewing trust decisions, I want the allow-list-to-ExpectPeer mapping and the remote-peer filter defined once on PeerPolicy, so that the TOFU fallback is visible, single, and ADR-able instead of buried mid-session.
14. As a developer pairing devices, I want an empty allow-list to refuse connections to unpaired devices by default, so that the explicit-pairing promise in the glossary holds.
15. As a daemon operator, I want engine crashes to be observed by the supervisor through real engine health rather than a sleep loop, so that a crashed engine restarts with backoff.
16. As a maintainer, I want the fake supervision task and its abort helpers deleted once engine health is wired, so that no dead machinery remains to mislead readers.
17. As a CI author, I want `cargo fmt --all --check` to pass, so that formatting gates stay green.
18. As an automated test harness, I want daemon stop and status testable against a temp home directory without spawning a real process, so that lifecycle tests run fast and deterministically.
19. As an automated test harness, I want unambiguous exit codes and status payloads from `daemon stop`, so that CI scripts can cleanly assert daemon termination.
20. As a maintainer of the reconciliation engine, I want one ancestor-walk helper called for both sides, so that lineage logic has a single shape to test.
21. As a developer reading the reconciliation code, I want the base-resolution interface to tell the truth about its outputs, so that I do not hunt for a None path that cannot happen.
22. As a developer recovering from a rollback, I want every file on both sides treated as a preserved addition when lineage is broken, so that degraded mode is non-destructive by construction.
23. As a future contributor, I want the store to remain the single source of truth for chunker configuration, so that no caller re-derives store facts from the filesystem.

## Implementation Decisions

- **Single Store-opening interface.** ferry-folder owns opening a folder's store:
  it derives the folder master key from the config head, selects the cipher
  (ChaCha20-Poly1305 only), and returns a typed error on any failure. The silent
  PassthroughCipher fallback, the zero-FMK constant, and every call-site cipher
  choice are deleted. ferry-sync, the daemon supervisor, and ferry-scan all go
  through this one interface; none of them name a cipher.

- **One device-daemon entry point.** ferry-daemon exposes a single function that
  takes the Ferry home, the device identity, and the folder records, and runs
  signal handling, folder registration, the tick loop, and lock teardown. The CLI
  binary keeps argument parsing and delegates; its duplicated signal watch block,
  registration loop, and tick loop are deleted. This mirrors the ADR-0006
  PairingRitual amendment: one canonical entry, no parallel public workflow.

- **DaemonLock interface on ferry-platform.** The platform module gains a lock
  interface that owns the PID file: acquire, read_pid, is_running (defeating PID
  reuse via the existing process start token), and terminate with backoff polling
  up to a five-second deadline. The literal daemon PID filename is spelled in
  exactly one place. `daemon stop` deletes the PID and socket files only after the
  OS confirms exit; on timeout it reports an error and preserves the PID file.

- **Two-sided ancestor verification with empty-base degradation.** The
  reconciliation safety check receives local, remote, and base manifest
  identifiers. One ancestor-walk helper, called for each side, proves base
  reachable via parent pointers. If either walk fails, the base degrades to
  empty: all files on both sides are additions and nothing is pruned. The
  wall-clock timestamp fallback is deleted; a broken lineage never resolves to
  diffing against remote. The helper's return type states exactly what it can
  return, and the unused local parameter disappears because local is now load-
  bearing.

- **Trust policy on PeerPolicy.** PeerPolicy gains the remote-peer derivation
  (self-filtered device set) and the ExpectPeer resolution beside its existing
  config parser. The empty allow-list default becomes refuse-to-connect rather
  than TrustOnFirstUse, honoring the glossary's explicit-pairing rule and
  ADR-0002. Any TOFU behavior returns only behind an explicit config flag and a
  new ADR. The three copies of the self-filter walk are deleted.

- **Supervisor stops re-deriving folder facts.** The supervisor's DefaultHasher
  polynomial guess and its direct `.ferry/config` walk are deleted. It consumes
  the folder-opening interface, which returns the store's real polynomial, and
  leaves peer-policy resolution to the sync engine. The store remains the source
  of truth (ADR-0001).

- **Honest supervision.** Engine health is surfaced from the engine handle into
  the supervisor's tick, so a crashed engine restarts with the existing backoff.
  The placeholder sleep-loop task, its abort helper, and its finished-check helper
  are deleted. Restart accounting stays internal to the supervisor.

- **Universal initialization guard.** ferry-folder exposes a directory inspection
  that answers "is this path an initialized Ferry folder" in one call. TUI and
  GUI folder-addition flows invoke it before dispatching registration; an
  uninitialized path is blocked with an inline banner pointing at `ferry init` or
  `ferry pair`. The web UI's existing check delegates to the same inspection.

- **Formatting gate restored.** The duplication driving the `cargo fmt` failures
  is deleted by the daemon consolidation; no formatting-only churn elsewhere.

## Testing Decisions

- **Good tests assert external behavior only**: reconciliation outcome manifests,
  CLI exit codes and output payloads, UI validation states, and error types from
  the folder-opening interface. Never internal loop counters, task handles, or
  helper state.

- **Seams, fewest and highest first.** Four seams, all but one pre-existing:
  1. The folder-opening interface in ferry-folder: tests assert key-unwrap failure
     is a loud typed error and that no plaintext path exists.
  2. The DaemonLock interface on ferry-platform plus the daemon entry point:
     stop/status are tested as pure functions of a temp home directory.
  3. The ConvergenceEngine seam for reconciliation: rollback tests drive
     bidirectional lineage through the existing engine-level harness.
  4. The PeerPolicy interface: trust-resolution tests through the engine.
  No new seams beyond these; the deleted call-site policies needed seams that the
  consolidation removes.

- **Modules under test**: ferry-folder (store opening, initialization inspection),
  ferry-platform (DaemonLock), ferry-sync-engine (bidirectional lineage, empty-
  base degradation), ferry-sync (PeerPolicy resolution, refuse-unpaired default),
  ferry-daemon (supervisor restart on real engine health, daemon entry), ferry-cli
  (stop timeout reporting, status coherence, PID preservation, exit codes),
  ferry-tui and ferry-gui (initialization guard states).

- **Prior art**: the anti-rollback tests for engine-level lineage assertions, the
  supervisor tests for restart and isolation behavior, the peer-policy tests for
  trust-resolution through the engine, and the pin-enforcement tests for
  at-rest encryption assertions.

## Out of Scope

- Force-killing daemons with SIGKILL automatically; operators escalate manually.
- Dynamic in-place folder re-initialization inside TUI or GUI pickers.
- Multi-generation conflict graph resolution beyond three-way ancestor traversal.
- Relay protocol changes, hosted relay discovery, and any transport work.
- Chunker selection or CDC benchmarking (ADR-0005 settled).
- Renaming or reorganizing crates; consolidation happens inside existing crates.
- Performance tuning of scan or reconcile paths.

## Further Notes

All changes preserve the v0 invariants: zero unencrypted network transfers,
authenticated at-rest storage, safe non-destructive reconciliation, deterministic
background sync. ADRs 0001 through 0006 are respected throughout; the one policy
reversal (TOFU default off) strengthens ADR-0002 rather than reopening it. The
deletion test governs scope: if a consolidation moves complexity instead of
concentrating it, it does not belong in this spec.
