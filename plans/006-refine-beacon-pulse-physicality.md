# 006 — Refine Status Beacon Pulse Physicality

- **Status**: DONE
- **Commit**: b5d3f21
- **Severity**: MEDIUM
- **Category**: Physicality & Origin
- **Estimated scope**: 1 file (`styles.css`)

## Problem

`@keyframes pulseBeacon` scaled down to `scale(0.6)` on each cycle. Collapsing to 60% of original scale created an exaggerated imploding motion that appeared jittery rather than like a soft optical light emitter.

```css
/* previous */
@keyframes pulseBeacon {
  0% { transform: scale(0.6); opacity: 0.8; }
  50% { transform: scale(1.2); opacity: 0.1; }
  100% { transform: scale(0.6); opacity: 0.8; }
}
```

## Target

Refine the breathing pulse to maintain resting geometry (`scale(0.92)` ➔ `scale(1.3)`) with a gentle opacity decay:

```css
/* target */
@keyframes pulseBeacon {
  0% { transform: scale(0.92); opacity: 0.7; }
  50% { transform: scale(1.3); opacity: 0.05; }
  100% { transform: scale(0.92); opacity: 0.7; }
}
```

## Steps

1. In `prototypes/fluid-glass/styles.css`, update `@keyframes pulseBeacon` with refined scale bounds.

## Verification

- **Feel check**: Observe the beacon ring in the Synced status. Confirm it produces a calm, ambient optical pulse without abrupt shrink transitions.
