# 13: Multi-Frontend Integration & Benchmark Acceptance Gate

**What to build:** An automated integration and verification suite that tests all frontends (CLI, TUI, Web SPA, and Native GUI) against the unified `UiBackend` seam, asserting cold-start latency, zero-CPU idle behavior, and clean compilation across all feature flag permutations.

**Blocked by:** 06 (Web UI Rewire), 07 (TUI Rewire), 08 (CLI Switch), 10 (GUI Widgets), 11 (GUI Pairing), 12 (GUI Conflicts)

**Status:** ready-for-human

- [x] End-to-end acceptance tests verify that all 4 frontends reflect synchronized state transitions simultaneously when files change.
- [x] Benchmarking confirms `ferry-gui` cold-start latency is sub-10ms and memory footprint is under 20 MB.
- [x] Automated profiling asserts 0.00% CPU usage across all UI frontends during 60-second idle periods.
- [x] Full workspace build checks (`cargo check --all-targets --all-features` and `--no-default-features --features lean`) pass with 0 warnings.
