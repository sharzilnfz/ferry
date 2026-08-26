# Performance findings — web layer (spec + tickets 01/02)

Reviewed by audit agent C. Both tickets landed during the run: ticket 01
(`crates/ferry-daemon/src/ui/{mod,status,actions,timefmt}.rs`, main.rs wiring)
and ticket 02 (`crates/ferry-daemon/assets/`). I reviewed the spec, the landed
Rust, and the landed JS/CSS. No code was changed.

What checked out clean:

- `/api/status` does not rescan. `manifest_id` comes from
  `EngineHandle::root_id()` (crates/ferry-sync/src/engine.rs:507-512), a
  short-lock read of the cached folder pointer. No GET path reaches
  `scan::one_shot` or the chunker.
- No lock crosses network or disk I/O. Handlers run in
  `spawn_blocking` (ui/mod.rs:305-315) and touch the engine only through
  short-lock getters. SSE is deferred with a 501 (ui/mod.rs:407-415), so no
  connection holds anything.
- Assets are `include_bytes!` at compile time (ui/mod.rs:31-33) with correct
  MIME types (ui/mod.rs:192-199). Nothing reads assets from disk per request.
- JSON responses buffer whole, which the spec allows; all of them are bounded
  documents. `conflicts.jsonl` compacts past 4096 lines
  (ferry-sync-engine/src/report.rs:124-135), so per-request reads stay cheap.

---

## Findings

### WEB-1 — every GET /api/status re-reads and re-decrypts the whole store index

- Location: crates/ferry-daemon/src/ui/status.rs:94 (call site);
  crates/ferry-store/src/store.rs:650-661 (cost)
- Class: disk-read-per-request
- Measured: `agreed_root_matches` calls `Store::open` on each request to fetch
  one agreed manifest. `Store::open` lists the index dir and does
  `fs::read` plus cipher-decrypt of EVERY `.ferryindex` container, merging the
  full location table, before it can serve a single blob. The bundled UI polls
  /api/status every 2s once SSE fails (which it does, see WEB-3), so this runs
  forever, and the cost grows with store history: each sealed index file adds
  work to every future status poll.
- Suggested fix: delete the per-request open. Open one read-only `Store` at
  startup into `UiState` (its mutable parts already sit behind mutexes), or
  cache the last `(manifest_id, current_root) -> bool` verdict so repeat polls
  skip the store entirely. Do NOT change the store layout for this; that is
  v0-frozen.

### WEB-2 — connectivity probe does blocking TCP dials per peer, per status request

- Location: crates/ferry-daemon/src/ui/status.rs:126-138
- Class: other (network-io-per-request)
- Measured: `probe_peer` runs `TcpStream::connect_timeout(.., 500ms)` for each
  peer inside `status_doc`. One dead peer means every 2s poll blocks 500ms;
  N stale peers serialize up to N x 500ms per request, forever. It runs on the
  blocking pool so the async runtime survives, but the dashboard's own poll
  loop hammers dead addresses indefinitely.
- Suggested fix: stop probing per request. Cheapest v0-honest option: report
  `"unknown"` unless an addr file exists and reuse the CLI's semantics only in
  `ferry status`. If reachability matters to the UI, cache a verdict with a TTL
  of tens of seconds and shrink the timeout well below 500ms.

### WEB-3 — polling fallback never engages when SSE answers 501

- Location: crates/ferry-daemon/assets/app.js:210-217
- Class: poll-storm (fallback logic; here the inverse: no fallback)
- Suspected: the code waits for two `onerror` events before degrading to
  polling. Per the EventSource spec, a response whose status is not 200 or
  whose Content-Type is not `text/event-stream` fails the connection after ONE
  error event and closes it for good. Today's server returns exactly such a
  501, so `sseErrors` stops at 1, `startPolling()` never runs, and the header
  stays "connecting…" with no live updates. Confirmation: run the daemon,
  open the dashboard, log `sse.readyState` from the console after the first
  error event; expect CLOSED (2).
- Suggested fix: treat `sse.readyState === EventSource.CLOSED` as terminal and
  call `startPolling()` immediately; keep the counter only for retryable
  network drops where readyState is CONNECTING.

### WEB-4 — SPA fallback serves HTML for unknown /api/* paths

- Location: crates/ferry-daemon/src/ui/mod.rs:184-189;
  contract at .scratch/web-dashboard/spec.md:52-53
- Class: spec-risk
- Measured: the fallback matches any unmatched path, including `/api/*`.
  Spec says unknown paths return the index "except paths starting with
  `/api/`". A typo'd API call gets a 200 `text/html` body instead of the
  documented `{error, code, hint}` JSON with a 404. Minor cost (an HTML body
  shipped for a JSON client), mostly a contract break.
- Suggested fix: three lines in `fallback`: if the path starts with `/api/`,
  return a 404 `ApiError`.

### WEB-5 — first GET walks the entire tree for scan counts; counts then never update

- Location: crates/ferry-daemon/src/ui/mod.rs:102-140; ui/status.rs:34, 51
- Class: rescan-per-request (once), spec-risk (stale fields)
- Measured: `scan_counts` is a `OnceLock`, so the first /api/status after
  startup does a full metadata-only readdir walk of the tree. No hashing, and
  it runs on the blocking pool, but on a large tree the first dashboard load
  stalls for seconds. After that the numbers are frozen for the process
  lifetime, and `bytes_chunked` is hardcoded 0 (ui/status.rs:51) because the
  engine exposes no per-tick scan stats.
- Suggested fix: fine to ship as-is for v0 if the tree is small; the real fix
  is a tiny engine accessor exposing last-tick `ScanStats` (the engine already
  computes them), which deletes both the walk and the hardcoded zero. That is
  an internal API addition, safe for v0, but touches ferry-sync so it needs an
  orchestrator call.

### WEB-6 — the spec's general rule invites a full rescan via the CLI status function

- Location: .scratch/web-dashboard/spec.md:38-40 vs spec.md:41-42 and :59-63
- Class: spec-risk
- Suspected: the general rules say handlers "call ferry-cli library functions
  directly", but the natural reuse target for /api/status,
  `ferry_cli::commands::status::run`, performs `crate::scan::one_shot`
  (crates/ferry-cli/src/commands/status.rs:46): a fresh policy-aware scan with
  hashing on every call. Ticket 01 dodged this correctly, but the next reader
  of the spec alone may not. The status section overrides the general rule
  only implicitly.
- Suggested fix: one sentence in the spec's /api/status section: do NOT route
  through `commands/status.rs`; its fresh-scan semantics violate the no-rescan
  rule. Contract-level edit, zero code.

### WEB-7 — spec claims pending_changes is a cheap metadata read; it cannot be

- Location: .scratch/web-dashboard/spec.md:64-66; implementation at
  crates/ferry-daemon/src/ui/status.rs:72-97
- Class: spec-risk
- Measured: pending_changes per docs/cli-json.md is a diff count against the
  agreed manifest. Computing it honestly requires reading a manifest blob from
  the store and diffing, proportional to entry count. The implementation
  approximates: compare the agreed manifest's root tree id to the cached root,
  answering 0 / -1 / null, which still costs one `Store::open` per request
  (see WEB-1). The spec's claim that these fields come from ".ferry/ state
  files, cheap metadata reads only" is not achievable as written.
- Suggested fix: spec should either bless the root-equality approximation
  explicitly (and note -1 covers divergence) or require the engine to cache
  the diff count per tick. Either way, name the store access so nobody solves
  it with per-request scans.

### WEB-8 — SSE has nothing to observe; deferral is right, the eventual design needs care

- Location: .scratch/web-dashboard/spec.md:134-136;
  crates/ferry-sync/src/engine.rs:751-755 (stdout-only emission);
  .scratch/web-dashboard/issues/01-api-server.md:46 (daemon-files-only scope)
- Class: spec-risk
- Measured: the engine emits STATE lines only via `println!` in
  `Ctx::status`. There is no watch channel, no broadcast, and
  `EngineHandle` exposes just `root_id`/`agreed_id`/`stats` short-lock getters.
  Ticket 01 may only touch ferry-daemon files, so a compliant SSE stream was
  impossible; the 501 deferral uses the escape hatch spec.md allows.
  The trap for whoever builds it later: polling `.ferry/` files or the handle
  once PER connected client would allocate and read per idle client, which
  spec.md:136 forbids.
- Suggested fix: when SSE lands, add a `tokio::sync::watch` (or mpsc broadcast)
  fed by one shared observer task that watches `root_id()`/`agreed_id()`, and
  fan out clones to clients. That means touching ferry-sync, so schedule it as
  its own ticket rather than squeezing it into a daemon-only pass. No wire
  format change involved.

### WEB-9 — agreement ledger parsed twice per status document

- Location: crates/ferry-daemon/src/ui/status.rs:82-88 and :99-104
- Class: other
- Measured: `most_recent_agreement` and `peer_rows` each call
  `AgreementLedger::list_folder`, re-reading and re-parsing the same file twice
  per request. Trivial today; free to fix while touching this file anyway.
- Suggested fix: read the records once at the top of `status_doc` and pass
  them down.

Also noted, not counted as perf findings: POST /api/share and
/api/pair/accept block up to 120s in `poll_for_file`
(ui/actions.rs:36, 261-281). User-initiated, on the blocking pool, no locks
held, so acceptable for v0; worth a comment if the UI ever fires them
automatically. And `main.rs` loopback enforcement (parse_ui_addr) matches the
spec's security stance.

---

## Ranked

| ID    | impact | effort | safe-for-v0-fix |
|-------|--------|--------|-----------------|
| WEB-1 | high   | low    | yes |
| WEB-2 | high   | low    | yes |
| WEB-3 | medium | low    | yes |
| WEB-4 | medium | low    | yes |
| WEB-6 | medium | low    | yes (doc edit) |
| WEB-5 | medium | medium | partially (v0: accept; real fix needs engine accessor) |
| WEB-7 | low-medium | low | yes (doc edit) |
| WEB-8 | low now, high later | medium | defer; new ticket for SSE |
| WEB-9 | low    | low    | yes |
