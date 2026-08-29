# 002 — Comprehensive Accessibility & Motion Preferences

- **Status**: DONE
- **Commit**: b5d3f21
- **Severity**: HIGH
- **Category**: Accessibility
- **Estimated scope**: 1 file (`styles.css`)

## Problem

The fluid glass prototype contained zero `@media (prefers-reduced-motion: reduce)` rules and zero `@media (prefers-reduced-transparency: reduce)` fallbacks, violating accessibility standards for vestibular motion sensitivity and visual clarity.

## Target

Introduce explicit `@media (prefers-reduced-motion: reduce)` to disable continuous keyframe looping and translate shifts while preserving essential color and opacity feedback. Add `@media (prefers-reduced-transparency: reduce)` with solid opaque contrast backgrounds.

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
    scroll-behavior: auto !important;
  }

  .beacon-ring {
    animation: none !important;
    opacity: 0.4 !important;
  }

  .sync-bar {
    animation: none !important;
    width: 100% !important;
    transform: none !important;
    opacity: 0.85 !important;
  }

  .flow-row {
    animation: none !important;
  }

  .is-blur-transitioning {
    filter: none !important;
    opacity: 1 !important;
  }

  .modal-card {
    transform: none !important;
  }
}

@media (prefers-reduced-transparency: reduce) {
  :root {
    --glass-bg: #141418;
    --glass-dock: #181820;
  }

  [data-theme="light"] {
    --glass-bg: #ffffff;
    --glass-dock: #f1f5f9;
  }

  .glass-main,
  .simulator-dock,
  .modal-card {
    backdrop-filter: none !important;
    -webkit-backdrop-filter: none !important;
  }
}
```

## Steps

1. Append accessibility media query blocks at the bottom of `prototypes/fluid-glass/styles.css`.

## Verification

- **Feel check**: Enable "Emulate CSS prefers-reduced-motion: reduce" in Chrome DevTools Rendering panel. Confirm that beacons stay steady without pulsing, sync progress is an accessible solid bar, and state transitions swap without blur/motion sickness triggers.
- Enable "Emulate CSS prefers-reduced-transparency: reduce" and confirm glass panels become solid legible surfaces.
