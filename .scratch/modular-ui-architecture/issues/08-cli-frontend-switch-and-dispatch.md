# 08: CLI Frontend Switch & Feature-Gated Dispatch

**What to build:** Update the `ferry ui` CLI command to accept `--web`, `--gui`, and `--tui` flags (or default to the best available compiled frontend), dynamically dispatching execution through `UiBackend` and printing actionable compilation guidance when a requested frontend was excluded at compile time.

**Blocked by:** 05 (Cargo Feature Flags)

**Status:** ready-for-agent

- [ ] `ferry ui` accepts `--web`, `--gui`, and `--tui` flags in `clap` parsing.
- [ ] If `ferry ui --gui` is run on a binary built without `--features gui`, it fails gracefully with exit code `feature-disabled` and prints: `"Feature 'gui' is not enabled in this build. Rebuild with: cargo build --features gui"`.
- [ ] Running `ferry ui` without explicit flags selects the preferred compiled frontend (GUI if enabled, falling back to Web, falling back to TUI).
- [ ] CLI test fixtures verify dispatch behavior and error messages across all feature combinations.
