# 003 — Eliminate transition: all on Interactive Components

- **Status**: DONE
- **Commit**: b5d3f21
- **Severity**: HIGH
- **Category**: Performance
- **Estimated scope**: 1 file (`styles.css`)

## Problem

`transition: all` was defined on `.btn-icon`, `.link-btn`, and `.pill`. `transition: all` forces the browser to monitor and interpolate every CSS property (including layout and paint properties) on every state change, which degrades rendering performance and causes unintended property jumps during reflows.

```css
/* previous */
.btn-icon { transition: all 0.14s var(--ease-out); }
.link-btn { transition: all 0.12s var(--ease-out); }
.pill { transition: all 0.12s var(--ease-out); }
```

## Target

Replace all `transition: all` declarations with specific, hardware-friendly properties:

```css
/* target */
.btn-icon {
  transition: transform var(--duration-fast) var(--ease-out), background-color var(--duration-fast) var(--ease-out), color var(--duration-fast) var(--ease-out), border-color var(--duration-fast) var(--ease-out);
}

.link-btn {
  transition: color var(--duration-fast) var(--ease-out), background-color var(--duration-fast) var(--ease-out);
}

.pill {
  transition: color var(--duration-fast) var(--ease-out), background-color var(--duration-fast) var(--ease-out), transform var(--duration-instant) var(--ease-out);
}
```

## Steps

1. In `prototypes/fluid-glass/styles.css`, replace `.btn-icon`, `.link-btn`, and `.pill` transition shorthand declarations with explicit CSS properties.

## Verification

- Inspect `.btn-icon`, `.link-btn`, and `.pill` in DevTools Styles panel; verify no `transition: all` rule is active.
- Verify hover and press states remain responsive and visually identical.
