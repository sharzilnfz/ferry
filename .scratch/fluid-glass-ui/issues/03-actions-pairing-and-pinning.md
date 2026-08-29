# Issue 03: Actions, Pairing Workflow, and Pinning Controls

Status: closed
Depends on: .scratch/fluid-glass-ui/issues/01-html-structure-and-tokens.md
Blocks: .scratch/fluid-glass-ui/issues/04-audio-haptics-theme-and-polish.md

## Description
Connect user interactive controls in `crates/ferry-daemon/assets/app.js` to daemon action endpoints: instant sync trigger (`/api/sync`), work protection/pinning (`/api/pin/start` and `/api/pin/stop`), pairing offer generation (`/api/pair/share`), and pairing offer acceptance (`/api/pair/accept`).

## Scope
1. Instant Sync Trigger:
   - Wire `btn-sync` to POST `/api/sync`.
   - Trigger the hardware-accelerated sync bar animation while request is in flight.
2. Work Protection (Pinning / Holding):
   - Wire `btn-pin` to toggle hold: POST `/api/pin/start` (with paths: `["*"]`) when idle, and POST `/api/pin/stop` when holding.
   - Wire `btn-release` to POST `/api/pin/stop` to release and merge held modifications.
   - Dynamically transition button visibility and label ("Hold Edits" / "Stop Hold" / "Release & Merge").
3. Pairing Workflow Modal:
   - Wire `btn-pair` to open the fluid glass pair modal.
   - Wire `btn-create-offer` to POST `/api/pair/share`. Display generated token / offer path, copy button, and secret warnings if secrets are detected.
   - Wire `btn-accept` to POST `/api/pair/accept` with user-entered offer payload and optional target directory.
   - Provide clear inline error and success feedback.
