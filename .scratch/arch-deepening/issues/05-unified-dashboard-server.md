# 05: Deep DashboardServer with Pluggable Backend Seams

**What to build:**
A single unified `DashboardServer` module managing Axum HTTP routes, static SPA assets, token authentication middleware, inactivity shutdown timers, and JSON `ApiError` responses, backed by a clean `DashboardBackend` interface.

**Blocked by:** 01: Low-Hanging Ponytail Reductions & Shared Platform Helpers

**Status:** done

- [x] Define the `DashboardBackend: Send + Sync + 'static` trait in `ferry-daemon/src/ui/mod.rs` (or dedicated shared module) exposing `get_status`, `list_conflicts`, `start_pin`, `stop_pin`, and `release_pin`.
- [x] Consolidate all Axum routing (`/api/status`, `/api/conflicts`, `/api/actions/*`), token authentication middleware, and SPA asset serving into `DashboardServer`.
- [x] Implement `DirectBackend` adapter wrapping in-memory daemon state.
- [x] Implement `IpcBackend` adapter delegating queries to the running daemon via IPC socket.
- [x] Add unit tests verifying route responses and auth middleware using an in-memory test fake backend.
- [x] Server module compiles and passes tests (`cargo test -p ferry-daemon`).
