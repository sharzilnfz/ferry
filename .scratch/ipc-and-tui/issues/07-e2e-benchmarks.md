# 07: E2E loopback integration suite and performance benchmarks

**What to build:** An end-to-end integration and performance benchmark suite verifying the complete headless daemon, IPC socket, CLI status queries, TUI live events, and ephemeral Web UI. Proves that idle CPU stays below 0.1% and memory RSS is reduced.

**Blocked by:** 04: CLI IPC integration, 05: TUI interactive actions, 06: Ephemeral on-demand Web UI.

**Status:** ready-for-human

## Implementation Notes for Agent
- Use `codebase-memory-mcp` to inspect existing E2E scripts under `scripts/quickstart-e2e.sh` and `scripts/dashboard-e2e.sh`.
- Create an automated test runner script `scripts/ipc-tui-e2e.sh`:
  1. Start `ferry-sync daemon` in headless mode.
  2. Query `ferry status --json` over IPC and verify matching schema.
  3. Modify local files in the watched tree, asserting that `DaemonMessage::StateChanged` is received within 100ms.
  4. Trigger concurrent edits to produce conflict files; verify TUI receives conflict event.
  5. Run `ferry ui --test` to confirm the ephemeral web server boots, answers status, and shuts down.
  6. Measure idle CPU and memory RSS of the headless daemon over 30 seconds, asserting CPU < 0.1%.

## Acceptance Criteria
- [x] `scripts/ipc-tui-e2e.sh` passes end-to-end in CI across macOS and Linux runners.
- [x] Idle daemon process CPU utilization is measured and verified at < 0.1%.
- [x] Full workspace unit and integration test suite (`cargo test --workspace`) passes with zero failures.
