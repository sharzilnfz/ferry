# ADR-0008: Transparent daemon auto-spawning for CLI and UI commands

Status: accepted (2026-09-01)

## Context

Running Ferry commands such as `ferry share`, `ferry join`, `ferry ui`,
`ferry tui`, or `ferry pin` previously required developers to maintain a
dedicated open terminal tab running `ferry daemon`. When the daemon was
offline, commands either failed with `daemon-not-running` errors or operated in
a degraded in-process mode that could not coordinate across separate processes
or listen for incoming peer connections.

Requiring manual daemon management adds developer friction and runs counter to
the zero-friction onboarding goal.

## Decision

- Commands that require background daemon services automatically check daemon
  liveness via the platform PID lock file and Unix domain socket / named pipe.
- If no active daemon is detected, Ferry transparently spawns `ferry daemon` as
  a detached background process.
- The initiating CLI/UI command waits for the daemon's IPC socket to become
  responsive (with exponential polling and timeout) before dispatching the
  operation.
- Explicit lifecycle control remains available via `ferry daemon start`,
  `ferry daemon status`, and `ferry daemon stop`.

## Consequences

- Developers can run single commands (`ferry share .`, `ferry ui`, `ferry tui`)
  out of the box without opening separate terminal tabs.
- Multi-process coordination (such as pairing listeners and mDNS discovery)
  reliably stays alive in the background after short-lived CLI commands exit.
- Process locks prevent multiple concurrent background daemons from colliding
  on the same `FERRY_HOME` state directory.

## Verification

- `crates/ferry-cli/tests/pin_cli.rs` and `daemon_lifecycle_tests.rs` verify
  that running commands in clean environments auto-spawns the daemon and
  succeeds cleanly.
- `ferry daemon status` and `ferry daemon stop` correctly query and terminate
  the auto-spawned background process.
