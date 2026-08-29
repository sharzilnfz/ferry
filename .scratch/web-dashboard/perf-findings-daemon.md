# Performance findings — daemon & transport steady state

Scope: `crates/ferry-sync-engine` (engine loop lives in `crates/ferry-sync/src/engine.rs`; the
`ferry-sync-engine` crate is the reconcile/execute library it calls), `crates/ferry-iroh`,
`crates/ferry-daemon/src/main.rs`. Target: near-zero idle CPU, flat RSS over hours.

## Measured baseline (TCP + iroh loopback pairs, empty trees)

Debug build, macOS, two daemons per pair, defaults (`--poll-ms 200`,
`--opportunistic-every 5`). Trees EMPTY unless stated. `ps -o %cpu,rss` sampled every 5s.

| Scenario | CPU / daemon | RSS | Notes |
|---|---|---|---|
| TCP pair, empty tree, ~65s idle | listener 0.4–0.8%, connector 0.7–2.8% | listener 9.9→11.5 MB (climbing), connector 9.5→10.3 MB | ~1 session/sec from opportunistic dialing (167 SESSION log lines) |
| iroh pair, empty tree, ~30s idle | both 0.4–0.9% | both FLAT: 27.2 / 27.6 MB | 7 and 6 OS threads; packs growing at ~5/s |
| TCP pair, **30 MB file** in tree, converged+idle | **~100% of one core, both daemons** | listener 11→**180 MB**, connector →121 MB | sustained; this is the steady state with any real content |

Disk while idle, both scenarios: **one new pack file + one new index file per completed tick**
(40 files after ~9s; 363 after ~70s; 852→860 in a later 20s window at the slower 30 MB-tree tick
rate). Store dir hit 2.8 MB in 70s on *empty* trees. Nothing prunes these.

---

## Findings

### DAEMON-1 — Tick body re-reads and re-chunks every byte of the tree, every 200 ms, forever

- Location: `crates/ferry-store/src/snapshot.rs:520-535` (`AdmittedKind::File => { let bytes = std::fs::read(path)...; chunk(...) }`), driven unconditionally from the poll loop at `crates/ferry-sync/src/engine.rs:1856-1868` via `Ctx::tick` (`engine.rs:840-853`) → `snapshot_source` = `snapshot_dir`.
- Class: idle-wakeup
- Measured: with one converged 30 MB file in the tree and zero further changes, each daemon burns **~100% of one core indefinitely** and RSS balloons to ~180 MB. Empty-tree ticks cost less but still stat/read-ignore-rules/walk every path 5×/sec. There is NO mtime short-circuit, NO watcher integration anywhere in the daemon path (`ferry-scan`'s incremental walker exists but nothing in ferry-sync/engine calls it — only ferry-cli does). Each tick also re-reads `ferry.ignore` from disk (`snapshot.rs:181`).
- Suggested fix: delete work, don't add machinery. Minimum viable: before chunking a file, compare `(len, mtime)` against the entry recorded in the current manifest's tree node for that path and reuse the stored chunk list when equal (the manifest already carries exactly these fields — no store-layout change). That converts an unchanged tick to a pure metadata walk. Longer term, gate the whole scan off directory mtimes or wire the existing incremental walker. A full watcher-based "skip the tick when clean" is the real fix but is bigger; either way the default 200 ms poll interval should also rise (see DAEMON-2). No wire-protocol or store-format impact.

### DAEMON-2 — Every tick mints a fresh root-manifest blob → one pack file + one index file per tick, forever

- Location: `crates/ferry-store/src/snapshot.rs:198-210` (manifest carries `created_sec/nsec` from the wall clock → unique bytes → unique BLAKE3 id → `put_meta` stages it); sealed by `store.flush()` at `snapshot.rs:210` into `seal_to_disk` (`crates/ferry-store/src/store.rs:950-981`), which writes a pack AND appends an incremental INDEX container per call.
- Class: unbounded-growth
- Measured: 5 pack files/sec + 5 index files/sec while idle on empty trees (40 packs at t=9s → 363 at t=70s → 850+ minutes in). At 30 MB content the rate tracks actual tick completion (~1–2/s) but never stops. File-count growth is unbounded; additionally `next_index_number` (`store.rs:998-1010`) does a full `read_dir` scan per append, so append cost grows linearly with hours of uptime, and cold start must union every index file ever written.
- Suggested fix: don't mint what you won't announce. In the tick path, skip `snapshot_dir`'s manifest write when the scanned root tree id equals the current pointer's root (compare before `put_meta`, e.g. let the walk return the root node without persisting the manifest, or hold the manifest write until `publish_scan` says Minted). That collapses idle disk writes to zero without touching format or wire. The adopt-and-hold logic in `publish_scan` (`engine.rs:415-440`) already wants exactly this distinction — the store write just happens too early. Safe for v0.

### DAEMON-3 — Opportunistic dialing runs a FULL session every second even when both sides are clean

- Location: `crates/ferry-sync/src/engine.rs:884-893` (`n.is_multiple_of(opportunistic_every)` ORs the divergence gate, default 5 × 200 ms = 1 Hz); session cost lands in `run_session_v1` (`engine.rs:955-1029`) — handshake + offers + exchange regardless of change.
- Class: idle-wakeup
- Measured: ~1 session/sec sustained on the idle pair (167 SESSION lines / 65 s in TCP mode; 191 in 30 s iroh mode). Each session is a dial, crypto establish, offer round-trip, agreement record — plus an agreement-ledger append? No: converged sessions short-circuit ("converged already" path, `engine.rs:1179-1213`) but only when agreed ids match; until then each round still records agreement and appends ledger bytes. This is most of the measured 0.4–2.8% idle CPU.
- Suspected (needs confirm): whether the every-Nth dial bypasses the convergence check repeatedly — the log shows repeated `SESSION complete: agreed on <same id>` pairs, meaning both sides keep re-recording the SAME agreement id once per second. Confirm by counting `.ferry/agreement` ledger growth over 10 min idle.
- Suggested fix: raise `DEFAULT_OPPORTUNISTIC_EVERY` substantially (e.g. 150 ≈ 30 s backstop) and/or skip the dial when `current_root == baseline_root && peer's last announced root == same` (the engine already has all three values under `FolderState`). Pure cadence change; no protocol impact.

### DAEMON-4 — Listener accept loop polls the iroh endpoint in 100 ms slices

- Location: `crates/ferry-iroh/src/transport.rs:446-481` (`IrohListener::accept`: `block_on(timeout(100ms, ep.accept()))` + `continue` on elapsed).
- Class: idle-wakeup
- Measured: not directly separable from the tick noise, but structurally this wakes the accept thread 10×/sec for the life of the process, plus two tokio worker threads (runtime built at `transport.rs:153-157`) that service the timers. Contributes to the iroh pair's nonzero idle floor.
- Suggested fix: lengthen the slice (e.g. 500 ms–1 s; shutdown latency budget allows it — the shutdown probe path tolerates seconds) or select on a `Notify` fired by `shutdown()` instead of timeout-polling the closed flag. Trivial, safe.

### DAEMON-5 — Path-sampler task ticks every 50 ms per live connection

- Location: `crates/ferry-iroh/src/transport.rs:368-393` (`spawn_path_sampler`, `tokio::time::interval(50ms)`).
- Class: idle-wakeup
- Suspected: sessions are short (~tens of ms), so exposure per session is small, but at DAEMON-3's 1 Hz dial rate this task spawns twice a second and spins 20 Hz while the connection lives. Cheap but pointless at this duty cycle. Confirm by instrumenting sampler iteration count.
- Suggested fix: sample once at connection open + once after upgrade (the observation is monotone latches — `selected_relay_seen`/`selected_ip_seen` never un-set), then exit on `conn.closed()`. Or drop the interval to 500 ms. Test-only observability feature costing production wakeups.

### DAEMON-6 — Main thread spin-parks in `join_until_signal`

- Location: `crates/ferry-sync/src/engine.rs:1938-1945` (200 ms sleep loop polling an AtomicBool).
- Class: idle-wakeup
- Measured: negligible CPU, but it's 5 pointless wakeups/sec of the main thread, forever.
- Suggested fix: park on a condvar notified by `shutdown()`, or simply `std::thread::sleep` long slices since SIGKILL/SIGTERM ends the process anyway (std has no signal handling here — the comment admits termination is signal-driven). One-line-class change.

### DAEMON-7 — Per-tick allocation churn: whole-file Vec, chunk vecs, manifest clones, Arc swap

- Location: `crates/ferry-store/src/snapshot.rs:521` (`fs::read` allocates file-size Vec per file per tick), `engine.rs:854-859` (fresh `manifest_bytes` + cloned manifest + new `Arc<SnapshotData>` per tick, replacing the old one in `FolderPointers::latest`).
- Class: buffer-alloc
- Measured: RSS trajectory on the 30 MB run (11→180 MB listener) shows allocator retention consistent with repeated multi-MB allocations; the empty-tree RSS climb (9.9→11.5 MB over 60 s) is the same effect at small scale. Old snapshot Arcs ARE dropped on replacement, so this is churn, not a leak — except during the discard window where `latest` holds the previous tick's data anyway (bounded at ~1 tick).
- Suggested fix: mostly evaporates if DAEMON-1 lands (no read when unchanged). If a buffer reuse is wanted independently: reuse one scratch Vec across `payload()` calls within a walk. Do NOT cache cross-tick buffers keyed by path — that's a memory leak wearing a perf costume.

### DAEMON-8 — `Store::get` reads the entire pack file to serve one blob

- Location: `crates/ferry-store/src/store.rs:782-795` (`std::fs::read(&path)` then footer parse, despite having `(plain_off, plain_len)`).
- Class: other (session-path efficiency)
- Suspected: not idle-path, but every REQ_META/REQ_DATA item served by the donor reads its whole pack (packs grow toward the seal target; staging flushes per burst make many small packs today, so it's masked). Will degrade as packs grow. Confirm: time `serve_data_request` with a store containing one large pack vs many small ones.
- Suggested fix: ranged pread at `(plain_off - header, plain_len + overhead)` bounds once pack layout exposes them; v0 can defer since current packs are small. Note only.

### DAEMON-9 — Session handler threads spawn per accepted/dialed session, joined only at shutdown

- Location: `crates/ferry-sync/src/engine.rs:1821-1843` (accept spawns per inbound conn), joins vec grows unboundedly until shutdown (`joins.lock().unwrap().push(h)` — entries popped only in `EngineHandle::shutdown`, `engine.rs:1922`).
- Class: thread-proliferation (mild)
- Measured: thread count stayed flat (3–7 threads observed) because handlers finish fast and permits cap concurrent handlers at 4 (`MAX_CONCURRENT_SESSIONS`, `engine.rs:336`). But the `joins` Vec accumulates one dead JoinHandle per session forever — at DAEMON-3's 1 Hz that's ~86k stale handles/day retained in memory. Small objects, but strictly growing RSS.
- Suggested fix: detach finished handlers (drop the JoinHandle after a try_join-style reap, or push handles to a bounded/reaped structure). Alternatively land DAEMON-3 first and the growth rate becomes irrelevant. Trivial either way.

### DAEMON-10 — iroh housekeeping floor is acceptable; relay/mdns costs are opt-in and dormant by default

- Location: `crates/ferry-daemon/src/main.rs:266-277` (relays/mdns wired only from flags); `crates/ferry-iroh/src/config.rs:12-13` (`RelaySetting::Disabled` default).
- Class: other (informational)
- Measured: default-mode iroh pair held flat RSS (27.2/27.6 MB over 30 s) and sub-1% CPU with no relay configured — no relay pings, no discovery churn, no mdns beacons unless `--relay`/`--discovery-mdns` are passed. With mdns enabled, `swarm-discovery` multicast cost was not measured (out of default path).
- Suggested fix: none needed for v0 beyond documenting that `--discovery-mdns` adds a periodic multicast beacon. Side note: usage text advertises `--discovery mdns` but the parser accepts only `--discovery-mdns` (`main.rs:192` vs the USAGE block at `main.rs:59`) — doc bug, not perf.

### DAEMON-11 — `ferry-sync-engine` crate itself is clean at steady state

- Location: `crates/ferry-sync-engine/src/*` (reconcile/plan/execute/report are pure per-session libraries; no threads, loops, buffers, or retained state of their own).
- Class: other (negative finding)
- Measured/Suspected: static review only — no spawn sites, no module-level caches; `report.rs` appends to `conflicts.jsonl` on disk rather than retaining history in memory. The only retained-state risk found anywhere is the `joins` Vec (DAEMON-9) and the store-side per-tick artifacts (DAEMON-2).

---

## Ranked summary

| ID | Impact | Effort | Safe-for-v0-fix |
|---|---|---|---|
| DAEMON-1 (full rescan+rechunk per tick) | Critical — 100% core at 30 MB; O(tree)/tick always | Medium | Yes (mtime/size reuse from existing manifest; no format change) |
| DAEMON-2 (manifest pack+index per tick, unbounded files) | High — unbounded disk/file-count, linearly slowing appends | Low–Medium | Yes (defer manifest write until Minted) |
| DAEMON-3 (full session every 1s while clean) | High — dominates idle CPU, redundant agreement records | Low | Yes (raise default N; gate on known-equal roots) |
| DAEMON-9 (joins Vec grows forever) | Low now, compounds with DAEMON-3 | Trivial | Yes |
| DAEMON-4 (100ms accept slices) | Low — constant 10Hz wakeup | Trivial | Yes |
| DAEMON-5 (50ms path sampler per conn) | Low | Low | Yes (sample-once semantics preserved) |
| DAEMON-6 (main-thread park poll) | Negligible | Trivial | Yes |
| DAEMON-7 (per-tick alloc churn) | Medium, mostly fixed by DAEMON-1 | Low (after DAEMON-1) | Yes |
| DAEMON-8 (whole-pack reads) | Deferred — hurts when packs grow | Medium | Defer to post-v0 |
| DAEMON-10 (iroh housekeeping) | None by default | n/a | n/a |
| DAEMON-11 (sync-engine crate) | None found | n/a | n/a |

Nothing above requires wire-protocol or store-layout changes; DAEMON-1's fix reuses fields
already in the manifest, DAEMON-2's moves an existing write later in time, DAEMON-3 changes a
default integer. All three together get idle state to: no disk writes, metadata-only walks,
sub-Hz sessions — which is where "near-zero idle CPU, flat RSS" actually lives.

*Empirical runs used `/tmp/ferry-audit-b` (removed) and debug builds; all processes killed.*
