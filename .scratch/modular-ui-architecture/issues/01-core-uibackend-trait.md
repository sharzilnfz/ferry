# 01: Core `UiBackend` Trait & Domain Type Definitions

**What to build:** Define the single, deep, asynchronous `UiBackend` trait and its typed domain models (`EngineSnapshot`, `ConflictEntry`, `PinRecord`, `ShareOffer`, `PairResult`, `PinStopSummary`, `PinReleaseSummary`, `UiEventStream`) in a shared foundation module. This establishes the unified seam across all frontends (CLI, TUI, Web SPA, and Native GUI) without circular dependencies or leaked platform details.

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] The `UiBackend` trait is defined with asynchronous methods for `get_status()`, `list_conflicts()`, `start_pin()`, `stop_pin()`, `release_pin()`, `share_initiate()`, `share_status()`, `pair_accept()`, `trigger_scan()`, and `subscribe_events()`.
- [ ] All method return types use strongly-typed domain structs rather than raw `serde_json::Value` objects.
- [ ] An in-memory fake backend implementing `UiBackend` is available for deterministic unit and integration testing without network or filesystem dependencies.
- [ ] Domain error taxonomy (`OpError`) cleanly categorizes errors with error codes, human messages, and actionable user hints.
