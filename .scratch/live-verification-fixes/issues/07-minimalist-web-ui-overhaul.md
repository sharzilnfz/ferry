# 07: Minimalist, Zero-Jargon Web Interface Overhaul (Captive Portal Style)

Status: ready-for-agent
Depends on: 04-api-status-peer-agreement-alignment.md, 05-async-pairing-workflow.md, 06-resilient-sse-streaming-polling-fallback.md
Blocks: 08-honest-token-auth-session-storage.md, 09-e2e-live-process-and-browser-verification.md

**What to build:**
A complete visual and interactive overhaul of the embedded web dashboard, adopting the ultra-minimal, high-efficiency, typography-driven style of the reference review designs. The dashboard removes all cryptographic jargon and raw hashes, replacing them with a massive hero status header (`SYNCED`, `HOLDING`, `CONFLICTS`, `OFFLINE`), human-centric status explanations, a two-column desktop layout (monospaced activity terminal on the left, streamlined control cards for Devices, Work Protection, and Conflicts on the right), full dark/light theme switching, and seamless mobile responsiveness.

**Blocked by:**
- 04: API Status Alignment & Synchronized Peer Agreement Badging
- 05: Asynchronous Non-Blocking Folder Pairing & Status Handshake
- 06: Resilient SSE Event Streaming with Silent Polling Fallback

### Acceptance Criteria

- [ ] Hero status section displays prominent bold status headings (`SYNCED`, `HOLDING`, `CONFLICTS DETECTED`, `OFFLINE`) with an illuminated status dot indicator.
- [ ] Subtitle copy uses plain English describing real-world device state (e.g. "All files up to date with 2 devices") with zero raw hashes, Merkle root pointers, or cryptographic terms.
- [ ] Left column displays a dark, monospaced Activity Feed showing real-time event logs with timestamps and status indicators.
- [ ] Right column presents clean, low-profile cards for "Connected Devices" (device names, status indicators, non-blocking share trigger), "Work Protection" (pinning controls and hold duration), and "Conflicts" (quarantine paths and inspect actions).
- [ ] Header includes a crisp theme toggle (sun/moon icon) switching between dark and light modes, persisting the selected preference in browser local storage.
- [ ] Layout collapses into a single column on mobile screen widths (390px) without horizontal scrolling or clipped text.
- [ ] Static HTML, CSS, and JS assets remain zero-dependency and compile directly into the daemon binary via `include_bytes!`.
