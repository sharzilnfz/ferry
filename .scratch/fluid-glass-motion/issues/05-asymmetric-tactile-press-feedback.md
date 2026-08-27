# 05: Asymmetric Tactile Press Feedback

**What to build:**
Implement asymmetric button transitions across `.btn`, `.btn-icon`, and `.pill`: instant compression on pointer-down (`:active` at 60ms) and natural smooth release (140ms).

**Depends on:** 07-consolidate-global-motion-tokens.md
**Blocks:** None

**Status:** closed

- [x] Configure `.btn:active` and `.btn-icon:active` with `transition: transform var(--duration-instant) var(--ease-out)` and `scale(0.97)` in `prototypes/fluid-glass/styles.css`.
- [x] Configure `.pill:active` with instant `scale(0.97)` press down feedback.
