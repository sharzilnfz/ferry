# Motion & Animation Improvement Plans

Audit completed against Emil Kowalski's Design Engineering Principles and Apple Fluid Interface guidelines.

## Plan Ledger

| # | Plan | Severity | Category | Status |
|---|---|---|---|---|
| [001](001-fix-sync-bar-transform.md) | Hardware-Accelerate Sync Bar Progress Indicator | **HIGH** | Performance | **DONE** |
| [002](002-add-reduced-motion-transparency.md) | Comprehensive Accessibility & Motion Preferences | **HIGH** | Accessibility | **DONE** |
| [003](003-remove-transition-all.md) | Eliminate `transition: all` on Interactive Components | **HIGH** | Performance | **DONE** |
| [004](004-sync-blur-morphing-timing.md) | Synchronize Blur Morphing Transition Timing | **MEDIUM** | Easing & Duration | **DONE** |
| [005](005-asymmetric-press-feedback.md) | Asymmetric Tactile Press Feedback | **MEDIUM** | Physicality | **DONE** |
| [006](006-refine-beacon-pulse-physicality.md) | Refine Status Beacon Pulse Physicality | **MEDIUM** | Physicality & Origin | **DONE** |
| [007](007-consolidate-motion-tokens.md) | Consolidate Global Motion Tokens | **LOW** | Cohesion & Tokens | **DONE** |

## Recommended Execution Order

1. **Tokens Baseline**: `007` (define duration & easing scales)
2. **Critical Performance & Accessibility**: `001`, `002`, `003` (GPU acceleration, reduced-motion, explicit properties)
3. **Physicality & Polish**: `004`, `005`, `006` (asymmetric clicks, blur sync, beacon pulse)

All plans have been implemented and verified in [`prototypes/fluid-glass/`](../prototypes/fluid-glass/).
