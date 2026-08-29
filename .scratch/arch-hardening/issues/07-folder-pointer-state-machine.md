# T-07: One owner for folder pointer state — serialize tick vs adopt, bounded accept, safe shutdown

Status: done
Depends on: T-06 (pin boundary settled)

`crates/ferry-sync/engine.rs` `Ctx` spreads folder state over eight mutexes
(latest, current, baseline, last_own_manifest_id, agreed, root, stats,
session_lock) written in different orders from poll ticks vs sessions. A tick
that started before a session adopt publishes afterwards and CLOBBERS the
adopted lineage (interleaving documented in spec B1); torn manifests get
offered as truth and become divergence baselines. Additionally: accept_loop
spawns an unbounded thread per inbound connection; my_offer/current_snapshot
spin-sleep 20ms x 10s while holding session_lock; EngineHandle::shutdown can
miss handlers that haven't registered yet (orphaned threads keep writing
during Drop).

Fix, staying std-threaded:
1. Collapse current/baseline/last_own_manifest_id (+root pointer) into ONE
   owned struct behind a single mutex — a small folder-pointer state machine
   with explicit operations (publish_own_snapshot, adopt_peer_manifest,
   baseline_for) that enforce ordering internally. Tick publication and
   session adoption both go through it, making clobbering impossible by
   construction.
2. Replace spin-sleep waits with Condvar (or restructure so callers don't
   wait under the session lock).
3. Bound concurrent accepted sessions (semaphore-style permit count, reject
   politely when busy) and register child handles synchronously before spawn
   returns so shutdown joins everything.
4. Make injectable the clock and the snapshot source in Ctx so tick logic
   (should-we-mint, publish-after-adopt) is unit-testable without real
   threads — this is the testability payoff; add tests that deterministically
   interleave tick-vs-adopt and assert no lost adoption and no torn offers.

Acceptance: new deterministic interleaving tests pass; convergence and
protocol_v1 suites green; shutdown test joins all threads before returning
(no writes after Drop observed via a probe).
