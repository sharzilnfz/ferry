# Performance findings — merged (A: core, B: daemon, C: web)

Merged from perf-findings-core.md, perf-findings-daemon.md,
perf-findings-web.md. Deduplicated, ranked by impact ÷ effort, grouped by
crate so fix agents stay file-disjoint. Full evidence lives in the three
source reports. All fixes below avoid wire-format and store-layout
changes (v0-frozen); anything that would touch them is marked DO-NOT-DO.

## Measured headline (audit B)

- Converged 30 MB tree, idle, defaults: ~100% of one core PER DAEMON, RSS
  to 180 MB. Cause chain: every 200 ms tick re-reads and re-chunks every
  byte (DAEMON-1/CORE-3 family), mints a fresh manifest → one pack + one
  index file per tick forever (DAEMON-2, unbounded disk), and opportunistic
  dialing runs a full crypto session every second while clean (DAEMON-3).
- Empty-tree idle: 0.4–2.8% CPU + 5 packs/sec written. Nothing prunes them.

## Fix groups

### Group STORE-IDLE — ferry-store/src/{snapshot,store,chunker}.rs

| ID | Finding | Impact | Effort | Fix |
|---|---|---|---|---|
| D1 (=B-1) | Tick body re-reads/re-chunks every byte every 200 ms; no mtime short-circuit in daemon path (snapshot.rs:520-535 via engine tick) | Critical | M | Before chunking, compare (len,mtime) vs current manifest's tree entry; reuse stored chunk list when equal → unchanged ticks become metadata-only walks |
| D2 (=B-2) | Every tick mints timestamp-fresh manifest → pack+index file per tick, unbounded (snapshot.rs:198-210, store.rs:950-981) | High | L–M | Skip manifest put_meta when scanned root tree id equals current pointer root; hold write until actually minted |
| C3 (=A-3) | Chunker rebuilds irreducibility proof + power table + out_table per FILE (chunker.rs:246-267) | High | S | Memoize derived tables per ValidatedPoly; reuse scratch read buffer across files |
| C5 (=A-5) | next_index_number readdir-scans ALL index records per sealed pack (store.rs:998-1009) | Med | S | Cache monotonic counter in Store, seed once on open |

### Group STORE-HOT — remaining ferry-store + ferry-scan

| ID | Finding | Impact | Effort | Fix |
|---|---|---|---|---|
| C1 (=A-1) | LocationTable::candidates linear-scans all entries per put/get → O(n²) sync (index.rs:503-509) | High | M | HashMap mirror beside `seen` (same pattern as existing dedup mirror) |
| C2 (=A-2, =B-8) | Every blob get reads entire pack + BLAKE3-hashes whole file (pack.rs:703 via store.rs:782) | High | L | Cache verified open-pack handle keyed by PackId (keep per-blob decrypt verify). Defer if time-boxed |
| C4 (=A-4) | find_entry linear Vec::find per rebuilt dir → O(entries²) (walk.rs:382,518; state.rs:43-50) | Med | S | Build HashMap<&str,&TreeEntry> once per rebuild_dir; delete find_entry |
| C7 (=A-7) | put_blob stats candidate packs under lock per put (store.rs:705-712) | Low | S | Negative-cache dangling pack ids |
| C9 (=A-9) | Deep clone of cached TreeNode per rebuilt dir (walk.rs:275) | Low | S | Remove + re-insert instead of clone |
| C10 (=A-10) | removal_keys Vec<String> contains() (reconcile.rs:480-488, ferry-sync-engine) | Low | S | HashSet (one line) |
| C6 (=A-6) | Engine core Mutex held across entire walk incl. hashing (scan/engine.rs:311,365) | Med | M | SKIP this session — medium refactor, contention latent until hashing parallelizes |
| C11 (=A-11) | staged-bytes linear scan + memcpy under store mutex | Low | M | SKIP this session |

### Group ENGINE-CADENCE — ferry-sync/src/engine.rs + ferry-iroh/src/transport.rs

| ID | Finding | Impact | Effort | Fix |
|---|---|---|---|---|
| D3 (=B-3) | Full crypto session every 1s while converged (engine.rs:884-893) | High | S | Raise DEFAULT_OPPORTUNISTIC_EVERY (~150 ≈ 30 s backstop) + skip dial when current_root==baseline==peer's last announced root |
| D9 (=B-9) | joins Vec grows forever, one dead JoinHandle per session (engine.rs:1821-1843) | Low | T | Reap/detach finished handles |
| D4 (=B-4) | iroh accept polls in 100 ms slices (transport.rs:446-481) | Low | T | Lengthen slice to 500ms–1s |
| D5 (=B-5) | 50 ms path sampler per connection (transport.rs:368-393) | Low | S | Sample once at open + upgrade, exit on conn.closed() |
| D6 (=B-6) | Main thread 200 ms park poll (engine.rs:1938-1945) | Negligible | T | Longer sleep slices |
| D7 (=B-7) | Per-tick alloc churn | Med | — | Evaporates with D1/D2; no separate work |

### Group WEB — ferry-daemon/ui/*.rs + assets/app.js

| ID | Finding | Impact | Effort | Fix |
|---|---|---|---|---|
| W1 (=C-1) | GET /api/status runs Store::open per request (re-reads + decrypts EVERY .ferryindex) at ui/status.rs:94 | High | S | Cache last (manifest_id,current_root)->bool verdict; never open store per request |
| W2 (=C-2) | 500 ms blocking TCP probe per peer per status request (ui/status.rs:126-138) | High | S | Report "unknown" unless addr file exists, or TTL-cache verdicts; shrink timeout |
| W3 (=C-3) | Polling fallback never engages on SSE 501 — EventSource closes after ONE error (app.js:210-217) | Med | S | Treat readyState===CLOSED as terminal → startPolling() immediately |
| W4 (=C-4) | SPA fallback serves HTML for unknown /api/* paths (ui/mod.rs:184-189) | Med | T | /api/* prefix → JSON 404 ApiError |
| W9 (=C-9) | Agreement ledger parsed twice per status doc (ui/status.rs:82-104) | Low | T | Parse once, pass down |
| W5 (=C-5) | First GET walks whole tree; counts frozen; bytes_chunked hardcoded 0 | Med | M | ACCEPT for v0 (real fix = engine accessor, separate ticket) |

### Spec-level doc fixes (done by orchestrator)

- W6: spec general rule invited routing /api/status through CLI status
  (fresh scan). Spec edited to forbid it explicitly.
- W7: spec claimed pending_changes is a cheap metadata read; it isn't.
  Spec edited to bless the root-equality approximation (-1 = divergence).

## Explicitly cleared (do not "fix")

- mtime short-circuit EXISTS in ferry-scan and is bench-proven; the gap is
  that the daemon tick path never calls it (that IS D1).
- diff_roots already prunes; no double-hash on apply; iroh housekeeping
  floor acceptable by default; ferry-sync-engine steady state clean.

## Deferred (post-session tickets)

- W8: SSE needs a tokio watch/broadcast fed by ONE observer task in
  ferry-sync (touches engine internals; its own ticket, not a drive-by).
- D8/C2 full ranged-pread pack reads when packs grow.
- W5 real fix: engine accessor exposing last-tick ScanStats.
