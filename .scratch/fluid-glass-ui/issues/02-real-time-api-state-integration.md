# Issue 02: Real-time API and State Morphing Integration

Status: closed
Depends on: .scratch/fluid-glass-ui/issues/01-html-structure-and-tokens.md
Blocks: .scratch/fluid-glass-ui/issues/04-audio-haptics-theme-and-polish.md

## Description
Integrate the daemon's real-time API layer (SSE streaming via `/api/events`, polling fallback on `/api/status`, conflicts on `/api/conflicts`, and bearer token handling) with the fluid glass state morphing engine in `crates/ferry-daemon/assets/app.js`.

## Scope
1. Auth & Session Management:
   - Extract `?token=` parameter from URL, persist to `sessionStorage`, and strip cleanly from address bar.
   - Attach `Authorization: Bearer <token>` to fetch requests.
   - Open token modal when HTTP 403 occurs.
2. State Morphing & Telemetry:
   - Synchronize Hero beacon, title, and state badge dynamically across 5 core daemon states: `synced`, `syncing`, `holding`, `conflict`, and `offline`.
   - Update the hairline telemetry bar with Root Hash (truncated 8-char hex with full hash on title), Held Edits counter, Conflict counter, Cipher (`Age-X25519`), and Transport (`QUIC` / `TCP`).
   - Dynamically render connected peer cards in `fleet-list` with live status dots (online/offline), peer identifier, transport channel, and agreement indicators.
3. Event Streaming & Resilient Polling:
   - Establish `EventSource` on `/api/events`.
   - Automatically fallback to bounded polling (1.5s interval) on SSE failure or 501 `not-implemented`.
   - Pipe incoming events cleanly into the live activity scroll feed (`activity-feed`).
