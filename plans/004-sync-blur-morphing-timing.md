# 004 — Synchronize Blur Morphing Transition Timing

- **Status**: DONE
- **Commit**: b5d3f21
- **Severity**: MEDIUM
- **Category**: Easing & Duration
- **Estimated scope**: 2 files (`styles.css`, `app.js`)

## Problem

In `app.js`, `applyState` applied `.is-blur-transitioning` and removed it with a fixed `setTimeout(..., 140)`. In `styles.css`, `.glass-main` had `transition: filter 0.16s var(--ease-out), opacity 0.16s var(--ease-out)`. The JS timeout was shorter than the CSS transition duration, abruptly clipping the blur effect before it could resolve smoothly.

## Target

Synchronize the CSS transition token (`--duration-base: 180ms`) with the JS class removal timer (`180ms`) so the crossfade and de-blur interpolate smoothly to completion.

```js
/* target in prototypes/fluid-glass/app.js */
if (mainCard) {
  mainCard.classList.add("is-blur-transitioning");
  setTimeout(() => mainCard.classList.remove("is-blur-transitioning"), 180);
}
```

```css
/* target in prototypes/fluid-glass/styles.css */
.glass-main {
  ...
  transition: filter var(--duration-base) var(--ease-out), opacity var(--duration-base) var(--ease-out);
}
```

## Steps

1. In `prototypes/fluid-glass/styles.css`, update `.glass-main` transition to use `var(--duration-base)`.
2. In `prototypes/fluid-glass/app.js`, set `setTimeout` to `180` in `applyState`.

## Verification

- **Feel check**: Click through simulator dock pills `1` through `5` rapidly and slowly. Confirm the blur crossfade is continuous, silky, and does not snap abruptly mid-transition.
