# 06: Rewire Web UI Server to Unified `UiBackend`

**What to build:** Refactor `DashboardServer` and its Axum HTTP route handlers in `ferry-daemon/src/ui` to consume `Arc<dyn UiBackend>` exclusively, removing all legacy backend trait clones and decoupling web routing from internal daemon data structures.

**Blocked by:** 03 (Daemon IPC Adapter), 04 (Push Event Streaming), 05 (Cargo Feature Flags)

**Status:** ready-for-agent

- [ ] `DashboardServer` holds `Arc<dyn UiBackend>` and delegates all API requests (`/api/status`, `/api/conflicts`, `/api/pin/*`, `/api/share/*`, `/api/pair/*`) through the trait.
- [ ] Legacy `DashboardBackend` trait and duplicate backend structs in `ferry-daemon/src/ui/backend.rs` are deleted in favor of the shared `UiBackend` interface.
- [ ] Existing SPA static assets (`index.html`, `style.css`, `app.js`) and token authentication middleware continue to function with 100% backward compatibility.
- [ ] Web UI integration tests (`tests/server_tests.rs`) pass cleanly when backed by either `FakeBackend` or `AutoBackend`.
