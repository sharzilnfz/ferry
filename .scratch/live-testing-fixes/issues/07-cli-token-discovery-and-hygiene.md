# 07: CLI Web UI token query command and workspace warning cleanup

**What to build:** Developers running the Web dashboard can query the active server URL and authentication token via a dedicated CLI command (`ferry ui token [folder]`), returning the authenticated browser URL or a structured error if no server is running. All workspace crates build and check cleanly with zero compiler warnings across macOS, Linux, and Windows.

**Blocked by:** 06: Fix TUI pin toggle on active pin and throttle disconnected daemon event stream

**Status:** complete

- [x] Web dashboard server records active session credentials to a local metadata file on startup and cleans it up on shutdown
- [x] Running `ferry ui token` outputs the full URL with valid query token when the Web UI is active
- [x] Running `ferry ui token` when no Web UI is running returns a structured error code
- [x] Unused imports in IPC and daemon crates are pruned
- [x] Platform-conditional helpers are cleanly annotated or pruned
- [x] `cargo check --all-targets` and `cargo test --workspace` produce 0 compiler warnings
