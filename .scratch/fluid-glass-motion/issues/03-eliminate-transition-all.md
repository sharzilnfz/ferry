# 03: Eliminate transition: all on Interactive Components

**What to build:**
Eliminate unconstrained `transition: all` declarations on `.btn-icon`, `.link-btn`, and `.pill`. Replace them with scoped transitions targeting only `transform`, `background-color`, `border-color`, and `color`.

**Depends on:** 07-consolidate-global-motion-tokens.md
**Blocks:** None

**Status:** closed

- [x] Refactor `.btn-icon` transition in `prototypes/fluid-glass/styles.css` to target explicit properties.
- [x] Refactor `.link-btn` transition to target `color` and `background-color`.
- [x] Refactor `.pill` transition to target `color`, `background-color`, and `transform`.
