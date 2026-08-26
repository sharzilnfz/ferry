# Ticket WIN-2: post-release lost-update race in pin_enforcement (CI red)

Status: ready-for-agent
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
