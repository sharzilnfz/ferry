# 01: Unify Sync Orchestration & Native Change Detection

**What to build:** Consolidate sync orchestration and transactional convergence into a single deep engine subsystem. Embed `ScanEngine` (with native OS filesystem watchers and debounce handling) directly within `SyncEngine` instead of running a 200ms sleep poll with raw snapshotting. Purge the dead `PinGate` seam from `ferry-materialize`.

**Status:** ready-for-agent

**Depends on:** None

**Blocks:** `.scratch/deep-sync-consolidation/issues/02-bind-ignore-policy-at-scan-seam.md`, `.scratch/deep-sync-consolidation/issues/05-self-healing-folder-engine-lifecycle.md`

- [ ] Embed `ScanEngine` as the internal change detection engine in `SyncEngine`
- [ ] Replace the 200ms `thread::sleep` poll loop with watcher event channels and debounce triggers
- [ ] Delete raw calls to `ferry_store::snapshot_dir_incremental` in sync loops, routing all local manifest updates through `ScanEngine`
- [ ] Remove obsolete `PinGate` trait from `crates/ferry-materialize/src/apply.rs`
- [ ] Ensure all existing matrix, roundtrip, and sync engine tests pass cleanly
