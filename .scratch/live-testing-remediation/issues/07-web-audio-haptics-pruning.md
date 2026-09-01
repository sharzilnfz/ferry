# Ticket 07: Web UI Scope Hygiene and Audio Synthesizer Pruning

Status: completed
Depends on:
Blocks: 10

## What to build

Clean up out-of-scope frontend additions identified during the spec review:

1. **Prune Web Audio Oscillator Synthesizer**:
   - In `crates/ferry-daemon/assets/app.js`, remove `playHapticFeedback` and associated Web Audio API synthesizers (ticks, snaps, alert waveforms) that were added outside the scope of `live-testing-fixes`.
   - If haptic feedback is desired on supported touch devices, use native browser `navigator.vibrate?.()` without audio oscillators, or prune completely to keep frontend assets minimal and focused.

2. **Frontend Asset Integrity**:
   - Verify that `crates/ferry-daemon/assets/app.js` and `index.html` render cleanly without JS console errors.

## Acceptance

- [x] `app.js` contains no unsolicited Web Audio API oscillator synthesis.
- [x] Web dashboard UI interactions (pairing, pin toggle, folder registration) operate smoothly without console warnings.
- [x] `cargo test -p ferry-daemon --test server_tests` passes.

## Comments

Resolves Spec finding #3 (scope creep in Web UI frontend assets).
