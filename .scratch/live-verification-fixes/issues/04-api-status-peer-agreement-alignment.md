# 04: API Status Alignment & Synchronized Peer Agreement Badging

Status: done
Depends on: None
Blocks: 06-resilient-sse-streaming-polling-fallback.md, 07-minimalist-web-ui-overhaul.md, 09-e2e-live-process-and-browser-verification.md

**What to build:**
Align the manifest identifier returned in dashboard status queries so peer agreement calculations accurately reflect synchronization state. Live daemon endpoints must return the signed manifest blob identifier consistently, matching the records stored in peer agreement ledgers. When two devices have synchronized their files, the status document and peer inspection badge must accurately report agreement, eliminating false "not agreed" warnings.

**Blocked by:** None (can start immediately)

### Acceptance Criteria

- [x] Status API responses populate `manifest_id` with the signed manifest blob identifier, aligning with agreement ledger records.
- [x] Peer rows in the status response accurately correlate the local manifest identifier against each peer's last-agreed manifest identifier.
- [x] When two nodes have synchronized to the same manifest, peer status evaluates to agreed, displaying an accurate green indicator.
- [x] When two nodes diverge or have pending changes, peer status accurately indicates divergence or unagreed state.
- [x] Automated HTTP tests assert that querying `/api/status` on synchronized nodes reports matching manifest identifiers and agreed peer states.
