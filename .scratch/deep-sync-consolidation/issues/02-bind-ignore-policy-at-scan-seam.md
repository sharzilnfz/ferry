# 02: Bind FerryIgnore Authoritatively at Scan Seam

**What to build:** Enforce that all sync manifest updates evaluate compiled `FerryIgnore` policies during directory scanning and watcher event handling. Eliminate the divergence between CLI scan and background daemon sync.

**Status:** ready-for-agent

**Depends on:** `.scratch/deep-sync-consolidation/issues/01-unify-sync-orchestration-and-change-detection.md`

**Blocks:** None

- [ ] Ensure `ferry-folder::open_folder` automatically loads and compiles `FerryIgnore` rules
- [ ] Bind `FerryIgnore` to `ScanEngine` in the unified `SyncEngine`
- [ ] Add integration tests verifying that files matching ignore patterns (e.g. `node_modules/`, `.env`, `target/`) are excluded from generated manifests and never synchronized over the wire
- [ ] Verify test suite passes cleanly across `ferry-ignore`, `ferry-scan`, and `ferry-sync`
