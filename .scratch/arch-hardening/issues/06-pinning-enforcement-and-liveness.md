# T-06: Session pinning enforced on the v1 path + real stale-pin liveness

Status: done
Depends on: T-05 (single apply path through the engine)

Three defects in session pinning:

1. **Unenforced on the v1 path.** `hold_filter` is consulted only in
`ferry-cli/src/exchange.rs`; the v1 engine session (`ferry-sync`, what the
real `daemon` binary runs) applies peer content with zero pin consultation —
the exact race pinning exists to prevent. Fix: consult pin state inside the
shared execution boundary of ferry-sync (the one place sessions mutate the
tree, after fetch, immediately before execute/materialize) so every driver
inherits enforcement. Expose it via the engine config, defaulting to a
no-pin policy.

2. **Pid-only liveness ⇒ immortal pins.** `PinRecord::liveness()`
(ferry-pin/src/pin.rs) checks kill(pid,0)/OpenProcess but never compares the
recorded `started_sec` against the process's actual start time, so pid reuse
makes a dead agent's pin live forever. Fix: record process start time
(platform-appropriate; ferry-platform may earn the helper) and treat
mismatched start time as Stale. Also fix the unix `pid as libc::pid_t` cast
(pin.rs:94): reject pids > i32::MAX as Stale instead of sign-flipping into a
process-group target.

3. **Non-atomic PinStore::start.** Load-check-write with a FIXED temp name
`pin-state.json.tmp` — concurrent starters clobber each other. Fix: unique
temp name (pid+random suffix) + atomic rename, and re-check `holding()`
after acquiring any filesystem-based serialization if cheap; at minimum make
the rename atomic and idempotent.

Also close the TOCTOU within the enforcing path: re-read pin state between
fetch completion and apply (cheap file read).

Acceptance: a test drives the ENGINE (not the CLI loop) with an active pin
matching a peer change and asserts the path is held/surfaced, then released
on pin end; liveness tests cover pid-reuse simulation (start-time mismatch →
Stale); two concurrent start() calls leave exactly one valid record.
