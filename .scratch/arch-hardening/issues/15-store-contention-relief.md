# T-15: Store contention — incremental index maintenance, no full-pack RAM loads

Status: ready-for-agent
Depends on: T-07, T-14 (exchange paths settled)

Two structural hotspots in ferry-store usage:
1. Every delivered pack triggers flush()+rebuild_index()
(ferry-sync/src/exchange.rs fetch_via_packs) — rebuild_index rescans ALL
packs (O(total store)) under the single global Mutex<Inner>, stalling
concurrent local scans (self-amplifying watcher Overflow -> full rescans).
2. build_pack_map loads every healthy pack's ENTIRE bytes into memory as
Arc<Vec<u8>> (ferry-sync/src/engine.rs, legacy path — confirm post-T-14 and
fix wherever it survives).

Fix pragmatically, keeping the pack format untouched:
1. Incremental index append: after ingesting a pack, insert just its blob
locations into the in-memory table and persist an incremental index record;
full rebuild becomes cold-start-only. Keep the global lock but shrink hold
times (do parsing/IO outside the lock, swap tables under it).
2. build_pack_map: stream packs (read footer/index region only) instead of
whole-file reads, or build the map from the location table directly.

Do NOT attempt lock-free redesign or split locks beyond what keeps scans
from starving (that can be follow-up); measure intent: add a debug log/metric
of longest lock hold per ingest stage so regressions are visible.

Acceptance: test ingesting K packs shows index cost proportional to K, not
total store size (assert rebuild_index called zero times during steady-state
ingest); large-pack ingest test passes with bounded peak memory; full store
suite + quickstart-e2e green.
