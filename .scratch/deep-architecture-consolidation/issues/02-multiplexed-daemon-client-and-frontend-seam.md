# 02: Multiplexed Daemon Client and Frontend Seam

**What to build:** Deepen the frontend communication seam in `ferry-ipc` and
`ferry-daemon`. Collapse the 16 granular `UiBackend` RPC methods into three
cohesive domain interfaces (`status`, `inventory`, `session`), replace the
per-method connect/disconnect `DaemonIpcAdapter` with ONE persistent
multiplexed socket connection (`DaemonClient`), add auto-reconnect with
backoff and transparent in-process fallback routing in `AutoBackend`, and
make `FakeBackend` satisfy the new seam so frontend tests substitute the
whole backend in-memory without spinning up socket servers.

**Blocked by:** None (01 landed first but shares no code paths).

**Status:** in-review

- [x] Split the flat 16-method `UiBackend` into `StatusDomain` (snapshots,
      conflicts, scan trigger, push-event stream), `InventoryDomain`
      (directory inspection, folder registry), and `SessionDomain` (pin
      lifecycle, share offers, pair accept, pairing sessions); `UiBackend`
      composes the three as a supertrait so every existing
      `Arc<dyn UiBackend>` caller keeps compiling.
- [x] Implement `ferry-ipc::client::DaemonClient`: one long-lived socket
      connection (Unix domain socket / Windows named pipe, existing
      newline-delimited JSON framing), pipelined request/response
      multiplexing with FIFO correlation, and background push-event fanout
      to subscribers. Wire protocol bytes unchanged.
- [x] Eliminate per-method connect/disconnect cycles entirely: status and
      inventory domains delegate to the shared connection; session-domain
      pin and pairing RPCs ride the same connection.
- [x] Auto-reconnect with exponential backoff (`ReconnectPolicy`, default
      2 attempts / 50 ms base for snappy offline fallback) plus a background
      supervisor that restores the connection after a loss so event streams
      resume without caller action.
- [x] Transparent in-process fallback routing in `AutoBackend` via one macro
      over the domains: route to `InProcessAdapter` only on the
      `daemon-unreachable` transport code, never on daemon domain errors
      (e.g. `pin-active`); share/pair file operations always use the
      configured in-process adapter.
- [x] Update `FakeBackend` to implement the three domain traits so frontend
      tests substitute the whole seam in-memory (no socket servers).
- [x] Rewire all callers (daemon dashboard server, GUI, TUI, CLI, tests) to
      the domain seam; delete the per-method connect boilerplate in
      `DaemonIpcAdapter` and the 16 pass-through delegation methods in
      `AutoBackend` (~350 LOC of adapter boilerplate removed, 468 insertions
      vs 820 deletions overall).
- [x] `cargo fmt --all`, `cargo clippy --workspace --all-targets` (zero
      warnings), `cargo test --workspace` (806 passed, 0 failed) all green;
      zero unsafe; CLI behavior unchanged.
