# Fluid Glass UI Integration Spec

## Overview
Migrate the `prototypes/fluid-glass/` interface into the main Ferry daemon assets (`crates/ferry-daemon/assets/`), replacing the legacy 2-column terminal/card layout with the unified fluid glass single-plane interface while preserving full real-time API integration (SSE streaming, polling fallback, token auth, peer state telemetry, pairing workflows, and work protection / pinning).

## Architecture & Layout
1. **Unified Glass Plane**: Single continuous frosted glass backdrop (`glass-main`) without nested boxes or visual noise.
2. **Hero Stage**: Dynamic status beacon (with optical ring breathing), live state badge, and direct primary action buttons (Sync Now, Hold Edits / Release & Merge).
3. **Hardware-Accelerated Sync Bar**: Smooth GPU-accelerated progress indicator (`transform: scaleX(...) translateZ(0)`) revealing during delta transfer.
4. **Hairline Telemetry Strip**: Instant visibility into Root Hash, Held Edits, Conflict count, Encryption cipher, and Transport channel (QUIC/LAN/Relay).
5. **Quiet 2-Column Content Flow**:
   - **Connected Devices**: Live peer list with connectivity dots (LAN / Relay / Reachable / Offline) and agreement badges.
   - **Live Activity Feed**: Timestamped chronological event stream with clear buffer action.
6. **Origin-Aware Pairing Modal**: Seamless share token creation (with secret scan detection) and peer offer acceptance.
7. **Auth Token Modal**: Unobtrusive token authorization for token-protected daemon sessions.
8. **Micro-Haptic Feedback & Keyboard Shortcuts**: Synthesized audio feedback (tick, snap, success, chime) and quick keys (`Space`, `P`, `T`, `Esc`).

## Tickets Breakdown
- `01-html-structure-and-tokens.md`: Integrate HTML layout and fluid glass CSS tokens into `crates/ferry-daemon/assets/`.
- `02-real-time-api-state-integration.md`: Bind real daemon SSE (`/api/events`) and polling (`/api/status`) telemetry into the state morphing engine.
- `03-actions-pairing-and-pinning.md`: Wire Sync, Pin/Unpin, Pair Offer Creation/Acceptance, and Conflict Quarantining.
- `04-audio-haptics-theme-and-polish.md`: Wire Web Audio haptics, theme toggle, keyboard shortcuts, and accessibility.
- `05-verification-and-tests.md`: Verify Rust asset embedding tests, end-to-end scripts, and visual fidelity.
