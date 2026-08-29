# 07: Rewire TUI Dashboard to Unified `UiBackend`

**What to build:** Refactor `ferry-tui` (`TuiApp`) to drive its state machine, keyboard action triggers, and live terminal updates through the shared `UiBackend` trait and `UiEventStream`, rather than binding directly to a raw IPC stream.

**Blocked by:** 03 (Daemon IPC Adapter), 04 (Push Event Streaming), 05 (Cargo Feature Flags)

**Status:** ready-for-human

- [x] `TuiApp` accepts `Arc<dyn UiBackend>` and subscribes to its `UiEventStream` for state rendering and conflict alerts.
- [x] Keyboard events (`r` for scan rescan, `p` for pin toggle, `c` for conflict view) call corresponding `UiBackend` methods.
- [x] Terminal guard and rendering loops continue to wake reactively on incoming events with zero polling overhead.
- [x] TUI test suite in `crates/ferry-tui/tests/` passes cleanly using the test `UiBackend` fake.
