# 02: Comprehensive Accessibility & Motion Preferences

**What to build:**
Introduce `@media (prefers-reduced-motion: reduce)` to disable continuous keyframe looping and translate shifts while preserving essential color and opacity feedback. Add `@media (prefers-reduced-transparency: reduce)` with solid opaque contrast backgrounds.

**Depends on:** None
**Blocks:** None

**Status:** closed

- [x] Add `@media (prefers-reduced-motion: reduce)` rules disabling beacon rings, sync progress loops, row translations, and blur morphing in `prototypes/fluid-glass/styles.css`.
- [x] Add `@media (prefers-reduced-transparency: reduce)` rules providing high-contrast opaque fallback colors for dark and light themes.
