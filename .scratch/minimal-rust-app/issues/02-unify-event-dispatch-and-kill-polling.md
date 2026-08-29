# Issue 02: Unify Event Dispatch and Eliminate Dual-Polling

Status: ready-for-agent
Depends on: .scratch/minimal-rust-app/issues/01-eliminate-redundant-disk-fallbacks.md
Blocks: .scratch/minimal-rust-app/issues/04-self-contained-rust-ui-app.md

## Problem
In `crates/ferry-daemon/assets/app.js` and `crates/ferry-daemon/src/ui/server.rs`:
- `/api/events` runs a 1-second timer that serializes status snapshots and diffs strings to push SSE updates.
- Concurrently, `app.js` executes `setInterval(loadStatus, 2000)` polling `/api/status`, creating redundant CPU wakeups and race conditions.

## Proposed Solution
- Update `/api/events` to subscribe directly to engine state transition broadcasts.
- Disable redundant `setInterval` polling in `app.js` when SSE is active and healthy; keep polling strictly as a fallback on disconnection.

## Acceptance Criteria
- SSE stream emits only on real state transitions without 1-second string-diffing loops.
- `app.js` pauses timer polling while SSE is connected.
- Zero CPU usage when the folder is idle.
