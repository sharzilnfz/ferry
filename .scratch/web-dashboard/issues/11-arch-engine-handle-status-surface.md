# Deepen EngineHandle into the status surface

Status: done
Depends on:
Blocks:

## Files

- `crates/ferry-sync/src/engine.rs` — `EngineHandle` (tag, agreed_id, root_id,
  stats, listen_addr, pinned_peers, scan_counts, pending_changes, peer_connectivity), `EngineStats`
- `crates/ferry-daemon/src/ui/mod.rs` — Deleted `scan_counts`/`count_tree` OnceLock walk, verdict cache, and probe cache
- `crates/ferry-daemon/src/ui/status.rs` — Streamlined `/api/status` to read directly from `EngineHandle`

## Problem

The dashboard needs live facts the handle does not expose, so it rebuilds each
one shallowly beside the engine that already computes it:

- Scan counts: the UI does its own metadata-only tree walk on first request and
  caches it for the process lifetime (`OnceLock`) — numbers go stale after the
  first poll tick, while the engine scans every 200 ms anyway.
- Convergence: the UI opens the store read-only and parses the agreed manifest
  to compare roots; when anything fails it reports `-1` (unknown). The engine
  holds both manifests in memory.
- Peer connectivity: the UI parses `.ferry/peers/*.addr` files and opens raw
  TCP probes — meaningless under iroh, where those addresses are stale loopback
  aliases; meanwhile `ferry-iroh` already records per-peer
  `PathObservation` (relay vs direct) nobody surfaces.

Three workarounds, three extra places reading state the engine owns: low
locality, and none of them testable except through HTTP.

## Solution

Grow `EngineHandle`: last-tick scan counts, pending-change count against the
most recent agreement, and a connectivity view fed by a tiny observer callback
the transport (or daemon wiring) updates. The status doc becomes cheap reads of
cached truth; delete the OnceLock walk, the store re-open, and `probe_peer`.

## Benefits

- One authority per fact; `/api/status` stops guessing from disk residue.
- Tests assert through the handle (`handle.scan_counts()`, `handle.pending()`)
  with no HTTP layer, matching how `root_id()` is tested today.
- The dashboard gains honest numbers (real pending counts, real path
  observations) instead of `-1` sentinels and forever-stale file counts.

## Before / after

```text
BEFORE (per /api/status request)        AFTER
UI walks tree once, caches forever      EngineHandle::scan_counts()
UI opens Store + parse_manifest         EngineHandle::pending_changes()
UI reads peers/*.addr + TcpStream       EngineHandle::peer_connectivity()
  connect_timeout(500ms)                  (fed from transport observations)
```

## Strength

Worth exploring

## Comments

Full analysis with diagrams: /var/folders/y9/hnkm2lv91n5chc4116wp_hf40000gn/T/architecture-review-1787745437.html (architecture audit A0, 2026-08-26).

Implementation report (2026-08-26):
- Grew `EngineHandle` with `scan_counts()`, `pending_changes()`, `peer_connectivity()`, and `record_peer_connectivity()`.
- Updated `SyncEngine::start` to load the baseline agreement from disk on startup, and `FolderState::publish_scan` to record per-tick `ScanStats`.
- In `crates/ferry-daemon/src/ui/mod.rs` and `status.rs`: deleted `OnceLock` metadata walk (`count_tree`), `Store::open` per-request manifest parsing, and blocking `probe_peer` TCP checks.
- Fixed the `pending_changes` destructuring bug where peer device IDs were passed as manifest IDs.
- Ensured unknown `/api` and `/api/*` routes return JSON 404 `ApiError`.
- Verified `scripts/dashboard-e2e.sh` passes 100% green and all unit/integration tests pass.

