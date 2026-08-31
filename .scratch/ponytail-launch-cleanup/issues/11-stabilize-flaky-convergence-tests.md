# Ticket 11: Stabilize flaky sync-convergence tests

Status: needs-triage
Depends on:
Blocks: noise-free `cargo test --workspace`

## What to build

Four test binaries flake under full-workspace parallel load with fixed
convergence timeouts. All pass in isolation; failures are timeouts, not
assertions:

- `ferry-iroh --test relay_forced` (2 tests, 90s-class waits; "markers never
  landed on b after convergence")
- `ferry-sync --test reconciliation_quarantine` (line 29 wait; observed
  FAIL/PASS/FAIL across three isolated runs, ~2/3 failure rate locally)
- `ferry-sync --test ignore_policy_sync` (line 231, "no convergence within
  30s")
- `ferry-cli --test exchange_loopback` (line 221, "deletion never reached B"
  after a 90s timeline)

Options to evaluate: poll-with-backoff instead of fixed deadline, retry-once
wrapper, serial execution for the convergence suite, or a shared
`await_convergence` helper with a generous ceiling and early-exit.

## Acceptance

- [ ] Each listed binary passes 10 consecutive isolated runs
- [ ] `cargo test --workspace --no-fail-fast` green on an idle machine

## Comments

Found while verifying ticket 10; unrelated to that fix (ferry-sync has no
dependency edge to ferry-ipc, confirmed via `cargo tree`). Recorded in
ticket 10's comments with panic lines and timings.
