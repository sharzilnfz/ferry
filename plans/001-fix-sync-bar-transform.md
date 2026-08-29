# 001 — Hardware-Accelerate Sync Bar Progress Indicator

- **Status**: DONE
- **Commit**: b5d3f21
- **Severity**: HIGH
- **Category**: Performance
- **Estimated scope**: 3 files (`styles.css`, `app.js`, `index.html`)

## Problem

The sync progress indicator `.sync-bar` animated the layout property `left: -35%` to `left: 100%` using `ease-in-out`. Layout properties trigger recalculate styles, reflow, and repaint on every animation frame, degrading frame rates during active sync sessions on low-power devices.

```css
/* prototypes/fluid-glass/styles.css:402 — previous */
.sync-bar {
  position: absolute;
  left: 0;
  top: 0;
  bottom: 0;
  width: 35%;
  background: var(--state-syncing);
  animation: syncMove 1s infinite ease-in-out;
}

@keyframes syncMove {
  0% { left: -35%; }
  100% { left: 100%; }
}
```

## Target

Hardware-accelerate the animation using `transform: translateX()` and continuous linear interpolation. Additionally make the container `.sync-track` expand smoothly when visible instead of jumping via `display: block`.

```css
/* prototypes/fluid-glass/styles.css — target */
.sync-track {
  width: 100%;
  height: 3px;
  background: var(--border-subtle);
  border-radius: 3px;
  overflow: hidden;
  position: relative;
  margin-top: 6px;
  opacity: 0;
  max-height: 0;
  transition: opacity var(--duration-fast) var(--ease-out), max-height var(--duration-fast) var(--ease-out), margin-top var(--duration-fast) var(--ease-out);
}

.sync-track.visible {
  opacity: 1;
  max-height: 4px;
  margin-top: 6px;
}

.sync-bar {
  position: absolute;
  left: 0;
  top: 0;
  bottom: 0;
  width: 35%;
  background: var(--state-syncing);
  transform: translateX(-100%);
  animation: syncMove 1.1s infinite linear;
}

@keyframes syncMove {
  0% { transform: translateX(-100%); }
  100% { transform: translateX(300%); }
}
```

## Steps

1. Update `.sync-bar` and `@keyframes syncMove` in `prototypes/fluid-glass/styles.css` to use `transform: translateX()`.
2. Update `.sync-track` to use `opacity` and `max-height` transitions with `.sync-track.visible`.
3. In `prototypes/fluid-glass/app.js`, toggle `syncTrack.classList.toggle("visible", mode === "syncing")`.
4. In `prototypes/fluid-glass/index.html`, remove inline `style="display: none;"` from `.sync-track`.

## Boundaries

- Do not change the visual dimensions or colors of the sync track.
- Touch only `prototypes/fluid-glass/styles.css`, `app.js`, and `index.html`.

## Verification

- **Feel check**: Switch to state `2` (Syncing) or click "Sync Now". Confirm the progress bar slides smoothly across without frame drops or jitter.
- In DevTools Animations panel (10% speed), verify transforms animate entirely on the GPU compositor thread without triggering layout recalcs.
