# 05: Domain types for folder id and timestamps

**What to build:** Introduce a validated `FolderId` newtype and a `Timestamp` type to replace primitive-obsession findings from the deep-architecture-consolidation code review.

**Blocked by:** PR #1 (feat/deep-architecture-consolidation) merging.

**Status:** ready-for-agent

- [ ] `FolderId` newtype validated once at the boundary, replacing `validate_folder_id` (crates/ferry-folder/src/inventory.rs:243) and the inline `unhex::<16>` re-check (crates/ferry-folder/src/pairing.rs:339).
- [ ] `Timestamp` type bundling the `(i64, u32)` unix-seconds/nanos tuple currently threaded through `ConvergenceEngine::at`, `now_unix()`, and `record_held(...)`.
- [ ] Tests updated through public seams only.
