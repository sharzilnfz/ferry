# 04: Push Event Streaming & 0.0% Idle CPU Engine

**What to build:** An event-driven subscription pipeline that connects the sync engine's internal `tokio::sync::broadcast` stream directly to `UiBackend::subscribe_events` and Web UI Server-Sent Events (`/api/events`), replacing all 1-second interval timers and JSON string diffing with push notifications.

**Blocked by:** 01 (Core `UiBackend` Trait)

**Status:** ready-for-human

- [x] `UiBackend::subscribe_events()` returns a typed `UiEventStream` emitting live `UiEvent` items (state changes, transfer progress, and new conflict alerts).
- [x] The 1-second interval polling loop and JSON string diffing in `DashboardServer::api_events` is deleted.
- [x] The Axum `/api/events` endpoint streams SSE directly from the `UiEventStream`.
- [x] When no filesystem changes or sync exchanges occur, the event stream sleeps on OS socket selectors with 0.00% CPU utilization and zero wakeups.
