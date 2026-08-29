# Issue 04: Micro-Haptics, Theme Persistence, and Polish

Status: closed
Depends on: .scratch/fluid-glass-ui/issues/02-real-time-api-state-integration.md, .scratch/fluid-glass-ui/issues/03-actions-pairing-and-pinning.md
Blocks: .scratch/fluid-glass-ui/issues/05-verification-and-tests.md

## Description
Incorporate the Web Audio micro-haptic synthesizer, theme persistence, keyboard shortcuts, and accessible dialogs into the integrated dashboard.

## Scope
1. Micro-Haptic Audio:
   - Web Audio API synthesizer for tactile clicks (`tick`, `snap`, `success`, `alert`).
   - Sound toggle button (`btn-sound`) with persisted state in `localStorage`.
2. Theme Controller:
   - Theme toggle button (`btn-theme`) switching between light and dark modes.
   - SVG icon switching (sun / moon).
   - Theme state persisted in `localStorage` (`ferry_theme`).
3. Keyboard Shortcuts:
   - `Space`: Trigger Sync Now.
   - `P`: Open / Toggle Pair Modal.
   - `T`: Toggle Theme.
   - `Esc`: Close open modal / clear focus.
4. Polish & Accessibility:
   - Subtle blur morphing on state changes (`is-blur-transitioning`).
   - Appropriate ARIA roles (`role="dialog"`, `role="feed"`, `aria-live="polite"`).
