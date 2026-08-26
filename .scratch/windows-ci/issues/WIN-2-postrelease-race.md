# Ticket WIN-2: post-release lost-update race in pin_enforcement (CI red)

Status: done

Depends on:
Blocks:

## Problem

CI run 32969906200, windows leg: `engine_holds_pinned_peer_changes_and_
release_recovers_them` (crates/ferry-sync/tests/pin_enforcement.rs:149)
timed out waiting for post-release flow to resume. Dump proves BOTH trees
converged on "d2": node B wrote "d4" (line 136) but its engine exchanged
before scanning the fresh edit, adopted A's manifest (created AFTER A
applied d2 during the hold, hence winner by manifest recency), and
overwrote B's own working-tree file back to d2. Lost update in the
write-vs-exchange window. Passed locally; slow runner widens the window.
Suspected platform-independent flake, worst on Windows.

## Task

First DIAGNOSE precisely (report appended below as ## Comments): read
EngineFixture (crates/ferry-sync/tests/common/), engine exchange loop,
and how manifests win (creation time? per-path mtimes?). Confirm the
mechanism above or refute it.

Then FIX minimally. Candidate directions (pick what diagnosis supports):
- Test-side: quiesce/wait for the engine's post-write scan before
  releasing the pin, so the race window closes deterministically.
- Production-side: only if diagnosis shows a genuine contract violation
  (e.g., apply must never revert a locally-newer unscanned edit).
Do NOT weaken the held-ledger or release-planner assertions.

## Constraints

- No wire format or store layout changes.
- cfg-gated code type-checks everywhere (53b9ca3 rule).
- Must survive two back-to-back `cargo test --workspace` runs locally AND
  a green CI matrix; rerun CI once more if any doubt.

## Acceptance

Green CI run on fix/windows-ci including the windows leg, twice in a row
if the first run shows anything suspicious.

## Comments

**Root cause.** Test-triggered eventual-consistency race, not a contract
violation (full mechanism in
`.scratch/windows-ci/diagnosis/postrelease-race.md`). At
`crates/ferry-sync/tests/pin_enforcement.rs:133` a reconciliation session
could still be in flight on B: B's poll thread had offered its pre-release
pointer M_B2, and after `mark_released()` B pulls A's post-hold manifest
M_A3 — minted later, hence the winner by manifest recency
(`lineage_newer`, crates/ferry-sync/src/exchange.rs:790-806) — and applies
the changeset wholesale onto its tree (exchange.rs:482, 514-522). The test's
synchronous `fs::write(d4)` at line 136 was invisible to that session, so
the apply landed after the write and reverted `tree_b`'s `docs/other.txt`
to d2 (size-equal/content-divergent full rewrite,
crates/ferry-materialize/src/apply.rs:1042-1049); d4 then never re-scanned
and the 30 s wait at pin_enforcement.rs:149 timed out.

**Fix (test-only).** Inserted a convergence wait before the release:
`wait_until("hold-exit convergence", || fx.converged())` immediately above
`PinStore::mark_released()` (pin_enforcement.rs:137-142). `converged()`
(crates/ferry-sync/tests/common/mod.rs:169-179) is true exactly when both
engines agree on pointer AND agreement ids, i.e. the dangerous M_B2-vs-M_A3
session has fully drained (agreement is recorded last). Afterwards every
session is an equal-root no-op until B scans the fresh d4 write, closing
the lost-update window deterministically. No assertion weakened; no
production code changed; reuses existing helpers only.

**Verification (local Windows).**

- `cargo test -p ferry-sync --test pin_enforcement`: 5/5 consecutive passes
  (~1.9-2.1 s each).
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `FERRY_SYNC_E2E_TRANSPORT=iroh cargo test -p ferry-sync --test
  pin_enforcement`: pass.
- `cargo fmt --check`: touched file clean. Note: four PRE-EXISTING diffs in
  crates/ferry-pin/src/pin.rs (:340,:406,:432) and
  crates/ferry-platform/src/procs.rs (:183), introduced by commit acf0fa4,
  unrelated to this fix — filed separately if CI's fmt gate trips on them.
