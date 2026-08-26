# 06: Wire CLI and Daemon to Unified DashboardServer & Retire Cloned UI Handlers

**What to build:**
Connect both `ferry daemon --ui` and `ferry ui` to the deep `DashboardServer`, and delete the duplicate 750-line web UI implementation in `crates/ferry-cli/src/commands/ui/`.

**Blocked by:** 05: Deep DashboardServer with Pluggable Backend Seams

**Status:** done

- [x] Wire `ferry daemon --ui` to spawn `DashboardServer` using `DirectBackend`.
- [x] Wire `ferry ui` in `ferry-cli` to spawn `DashboardServer` using `IpcBackend`.
- [x] Delete `crates/ferry-cli/src/commands/ui/disk.rs` (364 lines), `crates/ferry-cli/src/commands/ui/error.rs` (170 lines), and `crates/ferry-cli/src/commands/ui/handlers.rs` (270 lines).
- [x] Ensure `ferry-cli` and `ferry-daemon` share single SPA asset embeddings.
- [x] Run full end-to-end acceptance tests verifying CLI `ferry ui` and background daemon `--ui` serve identical web dashboards.
- [x] Entire workspace builds and tests pass cleanly (`cargo test --workspace && cargo clippy --workspace`).
