# 05: Asynchronous Non-Blocking Folder Pairing & Status Handshake

Status: done
Depends on: None
Blocks: 07-minimalist-web-ui-overhaul.md, 09-e2e-live-process-and-browser-verification.md

**What to build:**
Refactor the web pairing workflow from a 120-second blocking synchronous HTTP call into an asynchronous, non-blocking flow. Initiating a share request via the web backend must immediately generate the pairing payload, return the short code, and report status as pending within milliseconds. The web interface and API can then query pairing completion asynchronously, allowing the browser interface to display the generated code and progress status without freezing HTTP worker threads.

**Blocked by:** None (can start immediately)

### Acceptance Criteria

- [x] `POST /api/share` initiates the pairing ritual and immediately returns the generated short code, offer payload location, and pending state within 50ms.
- [x] No HTTP thread or connection blocks synchronously waiting for the remote peer's pairing response file.
- [x] An asynchronous query or polling endpoint allows checking whether a pending pairing offer has been accepted by the remote peer.
- [x] When secrets are detected during the pre-share scan without explicit user override, the endpoint returns a structured warning list without generating pairing files.
- [x] Integration tests verify that initiating a share request returns an immediate response with valid short code metadata while the pairing ritual awaits completion in the background.
