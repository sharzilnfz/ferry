# 04: Atomic Convergence Engine

**What to build:** Combine three-way reconciliation, atomic file materialization, conflict quarantine, and agreement ledger commits into a single deep `ConvergenceEngine`. Eliminate the intermediate `ActionPlan` translation loop in `execute.rs`.

**Blocked by:** 01-deep-folder-inventory-module.md

**Status:** in-review

- [x] Define the `ConvergenceEngine` interface: `converge(local_tree, remote_manifest, base_manifest, store) -> ConvergenceResult`.
- [x] Merge three-way diff calculation and atomic temp-file materialization into a single transactional execution pipeline.
- [x] Encapsulate conflict quarantine suffix generation and `.ferry/conflicts.jsonl` logging within the convergence step.
- [x] Atomically commit the agreed manifest id to `AgreementLedger` upon successful materialization.
- [x] Delete the intermediate `ActionPlan` loop in `crates/ferry-sync-engine/src/execute.rs`.
- [x] Test convergence atomicity and rollback behavior directly at the engine seam.

Implementation notes (T-04 landing):

- New `crates/ferry-sync-engine/src/converge.rs` (`ConvergenceEngine` +
  free `converge()`); `execute.rs` (983 lines) and `plan.rs` (133 lines)
  deleted; the internal plan types now live crate-private inside
  `reconcile.rs` — callers never see intermediate action plans.
- Rollback discipline: verify-then-write (blob presence + loser
  region-verification before any disk write), temp+rename per file,
  `Overwrite::Expect` guard against the local manifest, report and
  ledger appended last.
- `AgreementLedger` commits the remote manifest id only when the run
  converged the tree exactly onto it (no conflicts, no held paths,
  nothing to send).
- Callers rewired: `ferry-sync/src/exchange.rs` (engine + `WireFetch`
  transport hook), `ferry-pin` (hold gate + `record_held` + release via
  converge; `split.rs`/`gate.rs` deleted), `ferry-cli` pin release,
  `ferry-sync/tests/pin_enforcement.rs`, sync-engine `matrix.rs` +
  `adversarial_fixture.rs`, and `ferry-pin/tests/pin_scenario.rs` — all
  now assert real filesystem outcomes through the engine seam.
