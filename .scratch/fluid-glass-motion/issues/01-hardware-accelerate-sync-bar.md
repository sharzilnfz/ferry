# 01: Hardware-Accelerate Sync Bar Progress Indicator

**What to build:**
Replace the layout property `left: -35%` -> `100%` on `.sync-bar` with hardware-accelerated `transform: translateX(-100%)` -> `transform: translateX(300%)` with continuous linear interpolation. Make `.sync-track` smoothly expand height and opacity via `.sync-track.visible` instead of abrupt `display: block` reflows.

**Depends on:** None
**Blocks:** None

**Status:** closed

- [x] Update `.sync-bar` and `@keyframes syncMove` in `prototypes/fluid-glass/styles.css` to use `transform: translateX()`.
- [x] Update `.sync-track` with `opacity`, `max-height`, and `margin-top` transitions.
- [x] In `prototypes/fluid-glass/app.js`, toggle `syncTrack.classList.toggle("visible", mode === "syncing")`.
- [x] In `prototypes/fluid-glass/index.html`, remove inline `style="display: none;"` from `.sync-track`.
