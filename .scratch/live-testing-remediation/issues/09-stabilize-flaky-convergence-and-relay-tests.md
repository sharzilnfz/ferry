# Ticket 09: Stabilize Flaky Sync-Convergence and Relay Test Suites

Status: ready-for-agent
Depends on: 01, 04, 06, 08
Blocks: 10

## What to build

Stabilize test suites that exhibit intermittent timeouts or flaky convergence under parallel load:

1. **`ferry-iroh --test relay_forced`**:
   - Address 90s timeout where markers fail to land on peer B during forced relay traversal.
   - Ensure relay client connection loop and endpoint dialing handle parallel port allocation without binding conflicts.

2. **`ferry-sync --test reconciliation_quarantine`**:
   - Replace fixed-deadline polling with an adaptive `await_convergence` polling loop that inspects store and manifest status every 10ms with backoff.

3. **`ferry-sync --test ignore_policy_sync`**:
   - Ensure ignore policy updates propagate through scanner and sync engine without waiting out 30s fallback timers.

4. **`ferry-cli --test exchange_loopback`**:
   - With Ticket 08's `has_local_wins` deletion fix and stdio pipe routing in place, ensure deletion propagation completes in <5s rather than timing out.

## Acceptance

- [ ] `cargo test -p ferry-iroh --test relay_forced` passes 10 consecutive isolated runs.
- [ ] `cargo test -p ferry-sync --test reconciliation_quarantine` passes 10 consecutive isolated runs.
- [ ] `cargo test -p ferry-sync --test ignore_policy_sync` passes 10 consecutive isolated runs.
- [ ] `cargo test -p ferry-cli --test exchange_loopback` passes 10 consecutive isolated runs.

## Comments

Pulled from ponytail-launch-cleanup Ticket 11 into live-testing-remediation spec.
