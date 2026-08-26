Status: done
Depends on: 04-e2e-live.md
Blocks:

# 07 — Perf fixes: dashboard web layer

Findings W1, W2, W3, W4, W9 from .scratch/web-dashboard/perf-findings.md
(evidence in perf-findings-web.md). Goal: /api/status becomes O(1)-ish per
poll — no store opens, no blocking dials — and the bundled UI actually
falls back to polling.

## Fixes

1. W1 HIGH — crates/ferry-daemon/src/ui/status.rs:94: `agreed_root_matches`
   calls Store::open per request (reads + decrypts EVERY .ferryindex).
   Cache the verdict keyed on (agreed_manifest_id, current_root) in UiState
   so repeat polls never open the store. Never change store layout.
2. W2 HIGH — crates/ferry-daemon/src/ui/status.rs:126-138: drop the
   per-request 500 ms blocking TcpStream probe per peer. Report
   connectivity "unknown" unless a `.ferry/peers/<peer>.addr` file exists,
   or TTL-cache probe results (>= 10 s) with a much shorter timeout.
   Spec allows "unknown"; docs/cli-json.md defines it.
3. W3 MED — crates/ferry-daemon/assets/app.js:210-217: EventSource closes
   permanently after ONE error on a non-200/non-event-stream response
   (today's SSE returns 501), so `sseErrors` stops at 1 and polling never
   starts. Treat readyState === EventSource.CLOSED as terminal →
   startPolling() immediately; keep the counter only for retryable drops
   where readyState is CONNECTING.
4. W4 MED/T — crates/ferry-daemon/src/ui/mod.rs:184-189: SPA fallback must
   return a JSON 404 ApiError ({error, code, hint} shape, code "not-found")
   for any path starting with /api/, per spec.md.
5. W9 LOW/T — crates/ferry-daemon/src/ui/status.rs:82-104: parse the
   agreement ledger once per status_doc, pass records down to both callers.

Do NOT take W5/W8 (engine accessor, SSE observer) — separate tickets later.

## Constraints

- Files: ONLY crates/ferry-daemon/src/ui/*, crates/ferry-daemon/assets/*,
  and crates/ferry-daemon/src/main.rs if wiring demands it. Other agents
  own all other crates today — stay out.
- No new dependencies. No npm tooling. clippy -D warnings clean.

## Verify

```
cargo clippy -p ferry-daemon --all-targets -- -D warnings
cargo test -p ferry-daemon
bash scripts/dashboard-e2e.sh
# manual: boot daemon with --ui, curl /api/status twice, confirm identical
# fast responses; open /api/nonsense → JSON 404 not HTML; load UI in a
# browser briefly to confirm polling engages (indicator shows polling).
```

## Comments

Landed 2026-08-26 (wave-1 agent). Note: a cancelled agent had already put
most of the fixes into the working tree without updating this ticket; each
was verified against status.rs/mod.rs before sign-off, and leftover debug
`eprintln!`s (UIDEBUG/UILEDGER/UIVERDICT) were stripped from status.rs.

- W1 done — `UiState::verdict_cache` (mod.rs) keyed on
  (`agreed_manifest_id`, `current_root`); `agreed_root_matches` (status.rs)
  opens the store only on the first poll of a given pair, cache-hit returns
  before any store access. Store layout untouched.
- W2 done — probe_peer keeps probing but TTL-caches verdicts for 30 s with
  a 100 ms connect timeout; no `.ferry/peers/<peer>.addr` on file reads as
  "unknown" per docs/cli-json.md.
- W3 done — app.js treats `readyState === EventSource.CLOSED` as terminal
  and starts polling immediately; the error counter only covers retryable
  CONNECTING drops. Verified in place.
- W4 done — axum fallback returns JSON `{error, code:"not-found", hint}`
  with HTTP 404 for every `/api/*` path; non-API unknown paths keep the SPA
  index fallback.
- W9 done — the agreement ledger is listed once per status_doc and the
  records are passed to both `pending_changes` and `peer_rows`.

Verify: `cargo clippy -p ferry-daemon --all-targets -- -D warnings` clean;
`cargo test -p ferry-daemon` 2/2 pass. Live TCP pair with `--ui` on both:
two fast `/api/status` polls byte-identical at ~12 ms (no store-open cost);
`GET /api/nonsense` → 404 JSON not-found; `POST /api/pin/start` → 200, a
second start → 409 pin-active, `/api/status` shows pin active+holding.

Known red outside this ticket's files: `scripts/dashboard-e2e.sh` fails its
final agreement assertion because the ENGINE never re-records an agreement
after the post-convergence tick (daemon logs show `root=40e1…` vs
`agreed=f106…` indefinitely on BOTH nodes, so `pending_changes` correctly
reads -1). The dashboard faithfully reports engine state; the divergence is
in ferry-sync/engine.rs / ferry-store snapshot territory owned by another
agent mid-flight. Not chased per wave instructions.
