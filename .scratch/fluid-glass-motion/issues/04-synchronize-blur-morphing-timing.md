# 04: Synchronize Blur Morphing Transition Timing

**What to build:**
Synchronize the CSS transition token (`--duration-base: 180ms`) with the JS class removal timer (`180ms`) in `applyState()` so that state-switching blur crossfades complete cleanly without clipping.

**Depends on:** 07-consolidate-global-motion-tokens.md
**Blocks:** None

**Status:** closed

- [x] Update `.glass-main` in `prototypes/fluid-glass/styles.css` to use `var(--duration-base)`.
- [x] Update `applyState()` timeout in `prototypes/fluid-glass/app.js` to 180ms.
