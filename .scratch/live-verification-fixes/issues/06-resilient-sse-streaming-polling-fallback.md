# 06: Resilient SSE Event Streaming with Silent Polling Fallback

Status: done
Depends on: 04-api-status-peer-agreement-alignment.md
Blocks: 07-minimalist-web-ui-overhaul.md, 09-e2e-live-process-and-browser-verification.md

**What to build:**
Deliver reliable real-time event updates to the web interface. The `/api/events` endpoint provides a server-sent events stream broadcasting engine state transitions as they occur. If event streaming is unsupported by the client network or drops unexpectedly, the client script silently and gracefully switches to 2-second background polling without emitting uncaught errors or stack traces in the browser console.

**Blocked by:** 04: API Status Alignment & Synchronized Peer Agreement Badging

### Acceptance Criteria

- [ ] `GET /api/events` establishes an active server-sent events stream, transmitting state transition events (`event: state`) whenever engine status changes.
- [ ] Connect events emit an initial state message reflecting the current synchronization status.
- [ ] Streaming connections do not leak resources, hold engine locks across network idle times, or crash on client disconnection.
- [ ] The browser client attaches listeners to the event stream, immediately updating live status indicators when events arrive.
- [ ] If `/api/events` returns an error or the connection terminates repeatedly, the client script smoothly falls back to 2-second polling without throwing uncaught exceptions in the console.
- [ ] HTTP tests verify SSE connection establishment, event framing, and clean teardown on client disconnect.
