# Issue 01: HTML Structure and Fluid Glass CSS Tokens

Status: closed
Depends on: none
Blocks: .scratch/fluid-glass-ui/issues/02-real-time-api-state-integration.md, .scratch/fluid-glass-ui/issues/03-actions-pairing-and-pinning.md

## Description
Port the single-plane glass morphology, ambient light gradients, and motion tokens from `prototypes/fluid-glass/` into `crates/ferry-daemon/assets/index.html` and `crates/ferry-daemon/assets/style.css`.

## Scope
1. Update `index.html`:
   - Ambient depth light elements (`glow-1`, `glow-2`).
   - Clean header with brand dot, connection indicator (`conn-pill`, `conn-beacon`, `conn-text`), sound toggle button (`btn-sound`), and theme toggle button (`btn-theme`).
   - Single unified glass plane (`glass-main`) containing Hero stage, Telemetry strip, and 2-column content flow.
   - Pair modal with tabbed or sectioned share/offer and accept interfaces.
   - Auth token modal for session token entry on 403.
   - Banner containers for conflict / share warnings.
2. Update `style.css`:
   - Import and consolidate Apple fluid interface design tokens and Emil Kowalski motion tokens (`--motion-duration-fast`, `--motion-easing-spring`, etc.).
   - Hardware-accelerated GPU transitions (`transform: scaleX(...) translateZ(0)`).
   - Glassmorphism backdrop filter support with `@media (prefers-reduced-transparency)`.
   - Comprehensive `@media (prefers-reduced-motion)` fallbacks.
   - Dark and light theme custom properties.
