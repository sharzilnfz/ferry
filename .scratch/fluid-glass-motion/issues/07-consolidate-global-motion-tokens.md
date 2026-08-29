# 07: Consolidate Global Motion Tokens

**What to build:**
Centralize duration scales (`--duration-instant: 60ms`, `--duration-fast: 140ms`, `--duration-base: 180ms`, `--duration-slow: 280ms`) and easing curves (`--ease-apple`, `--ease-out`, `--ease-in-out`) in `:root`, replacing ad-hoc duration literals across all component styles.

**Depends on:** None
**Blocks:** 03-eliminate-transition-all.md, 04-synchronize-blur-morphing-timing.md, 05-asymmetric-tactile-press-feedback.md

**Status:** closed

- [x] Define motion tokens in `:root` in `prototypes/fluid-glass/styles.css`.
- [x] Refactor all component transitions to consume shared CSS variables.
