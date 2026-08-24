# T-14: Retire the CLI's M0 exchange stack — one production sync loop

Status: ready-for-agent
Depends on: T-06 (pin enforcement lives in the engine), T-07 (stable engine
state machine)

ferry-cli/src/exchange.rs (~559 lines) + commands/daemon.rs implement a FULL
second exchange protocol (own accept-loop, HELLO string-splitting at daemon
.rs:216, own agreement settlement, reconcile wiring) while ferry-daemon runs
the v1 SyncEngine. Every cross-cutting feature must be wired twice (T-015's
pin hold_filter shipped on only one side). This fails the deletion test:
deleting it removes a class of bugs outright.

Fix:
1. Make the CLI's `daemon` command construct and run ferry_sync::SyncEngine
(the same thing ferry-daemon/main.rs runs) with equivalent configuration
derived from the CLI's folder/config plumbing.
2. Delete ferry-cli/src/exchange.rs and the hand-rolled serve_session/
spawn_dial_loop machinery in commands/daemon.rs.
3. Resolve the PeerState name collision: keep the reconciler's PeerState in
ferry-sync-engine; rename ferry-sync/engine.rs's offer-phase struct to
something accurate (e.g. OfferPhase/DivergenceCheck) since consolidation
makes coexistence pointless.
4. If commands (sync one-shot, status, pin) relied on pieces of the deleted
module, route them through the engine/public APIs; do NOT reimplement
protocol logic in the CLI. As part of this, move the CLI's hand-assembled
folder bootstrap (folder.rs: config-head parsing, FMK unwrap, Store::open,
polynomial lookup) behind ONE deep open-a-folder function (in ferry-sync or
ferry-store — your judgment, document choice in ticket comments) consumed by
both CLI commands and daemon startup.

Acceptance: scripts/quickstart-e2e.sh and skeleton-e2e.sh converge
byte-for-byte using ONLY the v1 engine path; rg confirms no HELLO/OFFER
message construction left in ferry-cli; `ferry status --json` schema
unchanged (docs/cli-json.md); pin start/hold/release works through the CLI
end-to-end.
