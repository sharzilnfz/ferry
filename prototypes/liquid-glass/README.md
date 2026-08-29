# Ferry UI Prototype: Liquid Glass & Minimal Futuristic

A clean, futuristic, ultra-minimalist UI prototype inspired by Apple's liquid glass design language, VisionOS, and macOS Sequoia.

## Design Philosophy

- **Liquid Glass Materials**: Frosted glass surfaces (`backdrop-filter: blur(28px)`), specular hairline highlights, subtle refractive gradients, and depth without clutter.
- **Ultra-Restrained Color Palette**: 98% monochrome (obsidian, graphite, crystal frost, crisp white typography, silver lines) with precision phosphor status beacons (Emerald for Synced, Topaz for Holding, Ruby for Conflicts, Ice Cyan for Syncing).
- **Distinct Character & Typography**: SF-style typography, -0.025em tracking, hairline stroke SVG icons, dynamic state transitions.
- **Three Radically Different Structural Variants**:
  1. **Variant A (`?variant=A`): Spatial Glass & Floating Dock** (VisionOS / macOS Sequoia floating glass cards + breathing glass core status + bottom dock).
  2. **Variant B (`?variant=B`): Obsidian Studio HUD** (Aerospace minimalism, split-view layout, high-density telemetry terminal, collapsible inspector).
  3. **Variant C (`?variant=C`): Dynamic Island & Bento Grid** (Morphing top dynamic island capsule + responsive glass bento tiles).

## Interactive Features

- **Live Backend + Mock Fallback**: Connects automatically to the Ferry daemon if running, or runs seamlessly in standalone mode with full live simulation.
- **State Simulator Toolbar**: Test all states instantly (`Synced`, `Syncing`, `Holding`, `Conflicts`, `Offline`, `Secrets Warning`).
- **Interactive Work Protection (Pin)**: Start holding, pause, release & merge with custom path filters.
- **Interactive Pairing / Share**: Generate pairing codes, simulate peer discovery, accept offer payloads.
- **Real-time Live Telemetry Feed**: Search, filter by level (`INFO`, `WARN`, `SUCCESS`, `ERR`), pause/resume, clear.
- **Dark / Light Mode**: Frosted obsidian glass vs. crystal frost glass.
- **Variant Switcher Bar**: Floating bottom switcher with keyboard left/right navigation (`←` / `→`).
