# 09: Native GUI Crate Bootstrap & Obsidian Theme Engine (`ferry-gui`)

**What to build:** Create and bootstrap the pure-Rust desktop application crate `crates/ferry-gui` using `eframe` and `egui`, setting up the obsidian dark fluid glass theme tokens, custom typography, and the main window frame.

**Blocked by:** 01 (Core `UiBackend` Trait), 05 (Cargo Feature Flags)

**Status:** ready-for-human

- [x] New crate `crates/ferry-gui` is added to the workspace members in `Cargo.toml`.
- [x] Obsidian dark glass visual tokens (background `#09090b`, frosted panels `rgba(18, 18, 24, 0.75)`, 1px glass borders, text hierarchy) are configured in `egui::Visuals` and custom style builders.
- [x] Application window initializes with a compact, modern desktop shell and window decorations.
- [x] Frame rendering executes reactively on state updates and user interactions with zero continuous busy loops.
