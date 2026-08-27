# 007 — Consolidate Global Motion Tokens

- **Status**: DONE
- **Commit**: b5d3f21
- **Severity**: LOW
- **Category**: Cohesion & Tokens
- **Estimated scope**: 1 file (`styles.css`)

## Problem

Transition durations (`0.12s`, `0.14s`, `0.16s`, `0.18s`, `0.25s`, `0.3s`) and easings were hardcoded arbitrarily across rules in `styles.css`. This made the interface feel subtly disjointed across different interactive subcomponents.

## Target

Establish a unified token hierarchy in `:root`:

```css
:root {
  /* Motion Constants */
  --ease-apple: cubic-bezier(0.16, 1, 0.3, 1);
  --ease-out: cubic-bezier(0.23, 1, 0.32, 1);
  --ease-in-out: cubic-bezier(0.77, 0, 0.175, 1);

  --duration-instant: 60ms;
  --duration-fast: 140ms;
  --duration-base: 180ms;
  --duration-slow: 280ms;
}
```

## Steps

1. In `prototypes/fluid-glass/styles.css`, define the duration and easing constants in `:root`.
2. Migrate all component rules (`.btn`, `.btn-icon`, `.link-btn`, `.flow-row`, `.pill`, `.glass-main`, `.beacon-dot`, `.beacon-core`) to reference token variables.

## Verification

- Search `prototypes/fluid-glass/styles.css` for raw duration numbers; verify interactive transitions consume standard CSS variables.
