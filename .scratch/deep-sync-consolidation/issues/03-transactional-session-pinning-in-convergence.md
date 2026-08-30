# 03: Transactional Session Pinning in Convergence Engine

**What to build:** Make `ConvergenceEngine` natively aware of session pin state, gating conflicting changes and atomically persisting `HeldLedger` in a single transactional step. Eliminate manual 4-step choreography in wire exchange callers and provide a clean `PinManager::release` interface.

**Status:** ready-for-agent

**Depends on:** None

**Blocks:** None

- [ ] Update `ConvergenceEngine` to accept folder state configuration, automatically check active pins, gate held paths, and write to `HeldLedger` during convergence
- [ ] Simplify `ferry-sync` wire exchange code to invoke convergence directly without manual matcher/ledger bookkeeping
- [ ] Provide transactional `PinManager::release` method that reconciles held entries, updates the tree, and clears the ledger atomically
- [ ] Add unit and integration tests verifying ledger persistence across simulated crashes and daemon restarts
