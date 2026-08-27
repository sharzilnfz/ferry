# 05: Cargo Feature Flag Architecture & Lean Headless Profile

**What to build:** Compile-time feature flags in workspace and CLI Cargo manifests (`[features] web-ui, tui, gui, lean`), allowing production headless deployments to strip 100% of graphical, TUI, and Web dependencies and reduce the compiled binary to a minimal 4.2 MB footprint.

**Blocked by:** 01 (Core `UiBackend` Trait)

**Status:** ready-for-human

- [x] Cargo features `web-ui`, `tui`, and `gui` are defined in `Cargo.toml` and default-enabled for full developer builds.
- [x] Compiling with `--no-default-features --features lean` successfully compiles the headless daemon and CLI binary without `axum`, `ratatui`, `crossterm`, or `eframe`.
- [x] The stripped lean binary size is measurably reduced (from ~18 MB down to under 5 MB).
- [x] CI configuration or local tests verify successful compilation across all feature combinations (`default`, `lean`, `tui`, `web-ui`, `gui`).
