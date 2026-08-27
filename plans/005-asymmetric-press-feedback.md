# 005 — Asymmetric Tactile Press Feedback

- **Status**: DONE
- **Commit**: b5d3f21
- **Severity**: MEDIUM
- **Category**: Physicality
- **Estimated scope**: 1 file (`styles.css`)

## Problem

Buttons used symmetric `0.14s` transition on both pointer-down (`:active`) and release. Real physical surfaces compress instantly under fingertip pressure and recover with natural spring elasticity; symmetric timing makes button presses feel rubbery and sluggish on initial click.

## Target

Implement asymmetric timing: instant/rapid feedback on `:active` (`--duration-instant: 60ms`) and graceful release (`--duration-fast: 140ms`).

```css
/* target */
.btn {
  ...
  transition: transform var(--duration-fast) var(--ease-out), background-color var(--duration-fast) var(--ease-out), color var(--duration-fast) var(--ease-out), border-color var(--duration-fast) var(--ease-out), opacity var(--duration-fast) var(--ease-out);
}

.btn:active {
  transform: scale(0.97);
  transition: transform var(--duration-instant) var(--ease-out);
}

.btn-icon:active {
  transform: scale(0.97);
  transition: transform var(--duration-instant) var(--ease-out);
}
```

## Steps

1. In `prototypes/fluid-glass/styles.css`, update `.btn:active` and `.btn-icon:active` to override transition duration with `var(--duration-instant)`.

## Verification

- **Feel check**: Click "Sync Now", "Hold Edits", and header icons. The press reaction should feel snappy and instantaneous on mousedown, with a smooth 140ms decompression upon release.
