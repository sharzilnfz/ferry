# 05: Self-Healing FolderEngine Lifecycle & Event Streaming

**What to build:** Encapsulate folder opening, polynomial validation, task supervision, internal exponential backoff crash restarts, and real-time `UiEvent` broadcast publishing inside `FolderEngine`. Simplify `Supervisor` into a clean registry map.

**Status:** ready-for-agent

**Depends on:** `.scratch/deep-sync-consolidation/issues/01-unify-sync-orchestration-and-change-detection.md`

**Blocks:** None

- [ ] Introduce `FolderEngine` encapsulating engine construction, task spawning, and internal crash recovery
- [ ] Wire `FolderEngine` to publish `UiEvent::StateChanged` and `UiEvent::Error` directly to the broadcast channel
- [ ] Refactor `Supervisor` to manage `HashMap<FolderId, FolderEngine>` as a lightweight registry
- [ ] Add tests verifying engine crash recovery and broadcast event delivery without full daemon harness
