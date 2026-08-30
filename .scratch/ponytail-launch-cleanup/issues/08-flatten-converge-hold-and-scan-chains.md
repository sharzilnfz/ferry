# 08: Flatten converge hold and scan chains

Status: ready-for-agent
Depends on: 03, 07
Blocks: 09

**What to build:** A converge and scan path that answers "where does this manifest decision come from?" in under three hops. From the user perspective a large tree still syncs with watcher-driven latency and no extra idle churn. From the maintainer perspective hold gating and tree walk logic live in one place.

**Blocked by:** 03, 07

**Status:** ready-for-agent

- [ ] One-caller wrappers `hold_matcher`/`record_held` and the `PathMatcher` wrapper over the ignore gitignore matcher are inlined into the convergence engine and the scan seam; the ignore policy is enforced once at the scan seam via `ScanEngine` so held and converged trees agree
- [ ] The convergence engine reconciliation returns held chunks directly so the gate plan needs no second tree walk; the backend's fluent config builder is replaced by struct literals; the TUI duplicated `match key.code` trees are collapsed to one async handler with a sync wrapper
- [ ] Deep chains are flattened: BFS guards collapse to one budget, dial takes an injected runtime handle instead of owning a runtime per instance, and supervisor plus folder engine bootstrap collapses to one watched-folder open per folder
- [ ] Verified through the convergence engine and scan engine seams: three-way reconciliation with hold, watcher-driven manifest updates respecting ignore rules, and backend key handling all remain green; deep-chain depth is at most three files per question

## Comments

Vertical slice that touches both the moved `pin` module (03) and the unified backend (07), hence blocks on both. Prefactors code to make later helper deduplication easy per "make the change easy, then make the easy change."
