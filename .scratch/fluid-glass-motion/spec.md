# Fluid Glass Motion & Animation Spec

## Overview
Elevate the Fluid Glass prototype (`prototypes/fluid-glass/`) to strict alignment with Emil Kowalski's Design Engineering philosophy and Apple Fluid Interfaces standards. All motion must be hardware-accelerated, accessible, physically consistent, and token-backed.

## Scope of Issues
1. **01 — Hardware-Accelerate Sync Bar Progress Indicator**: Migrate from layout `left` transitions to GPU `transform: translateX()`.
2. **02 — Comprehensive Accessibility & Motion Preferences**: Add `@media (prefers-reduced-motion)` and `@media (prefers-reduced-transparency)`.
3. **03 — Eliminate `transition: all`**: Scope component transitions to explicit properties.
4. **04 — Synchronize Blur Morphing Transition Timing**: Match JavaScript class removal timeouts with CSS transition durations.
5. **05 — Asymmetric Tactile Press Feedback**: Instant pointer-down response with smooth elastic release.
6. **06 — Refine Status Beacon Pulse Physicality**: Replace drastic scale collapse with subtle ambient optical breathing.
7. **07 — Consolidate Global Motion Tokens**: Centralize duration scales and custom easing curves in `:root`.
