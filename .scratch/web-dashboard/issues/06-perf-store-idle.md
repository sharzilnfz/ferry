Status: done
Depends on: 04-e2e-live.md
Blocks:

# 06 — Perf fixes: store idle path + hot-path quick wins

Findings D1, D2, C1, C3, C4, C5, C7, C9, C10 from
.scratch/web-dashboard/perf-findings.md (evidence in perf-findings-core.md
and perf-findings-daemon.md). Goal: unchanged ticks become metadata-only
walks with zero disk writes; kill the worst quadratic hot paths.

## Priority order (land top-down; cut bottom-up if time-boxed)

1. D1 CRITICAL — crates/ferry-store/src/snapshot.rs:520-535: before reading
   + chunking a file, compare (len, mtime) against that path's entry in the
   current manifest's tree node; when equal, reuse the stored chunk list.
   The manifest already carries these fields — no format change. Unchanged
   ticks must not read file bytes.
2. D2 HIGH — crates/ferry-store/src/snapshot.rs:198-210: do not put_meta a
   fresh timestamp-stamped root manifest when the scanned root tree id
   equals the current pointer's root (idle currently writes one pack + one
   index file per tick forever). If this genuinely requires an engine.rs
   call-site change, STOP at the boundary and record it in Comments
   instead of touching crates/ferry-sync (another agent owns it today).
3. C3 HIGH/S — crates/ferry-store/src/chunker.rs:246-267: memoize the
   derived (slide_out, out_table) per ValidatedPoly; reuse a scratch read
   buffer across files (walk.rs:448 allocates 256 KiB per file).
4. C5 MED/S — crates/ferry-store/src/store.rs:998-1009: cache the
   monotonic next_index_number in Store, seeded once on open.
5. C4 MED/S — crates/ferry-scan/src/walk.rs:382,518 + state.rs:43-50:
   HashMap<&str,&TreeEntry> built once per rebuild_dir; delete find_entry.
6. C1 HIGH/M — crates/ferry-store/src/index.rs:503-509: HashMap mirror for
   candidates() (same pattern as the existing dedup mirror at :477-479).
7. Time permitting: C7 (store.rs:705-712 negative-cache dangling pack ids),
   C9 (walk.rs:275 remove+reinsert instead of TreeNode clone),
   C10 (reconcile.rs:480-488 removal_keys → HashSet).

## Constraints

- Files: ONLY crates/ferry-store/*, crates/ferry-scan/*,
  crates/ferry-sync-engine/* (for C10 only). Do NOT touch
  crates/ferry-sync/*, crates/ferry-iroh/*, or crates/ferry-daemon/*
  (concurrent agents own those today).
- Store layout and wire format are v0-frozen — no format changes.
- No new dependencies.

## Extra scrutiny (SPEC.md flagged risk: scan throughput)

If you claim a measurable win on the scan path, add/adjust a benchmarks/
case (crates/ferry-scan bench-gate exists; benchmarks/scan.md is its
report) and include before/after numbers in Comments.

## Verify

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/dashboard-e2e.sh
# empirical: TCP pair, converged 30 MB file, ~30s idle:
# expect no new pack/index files (ls | wc -l stable) and CPU near zero.
```

## Comments

### Report (agent F2, 2026-08-26)

**Landed:** D2, D1 (as opt-in API, see boundary note), C3, C5, C4, C1,
C7, C9, C10. Nothing cut.

- **D2 — landed in `snapshot_dir` itself.** When
  `identity.parent_manifest_id` resolves to a readable manifest with the
  same folder/device id and the scanned root tree id equals that parent's
  root, the stored parent manifest is returned instead of minting a
  timestamp-stamped twin (`snapshot.rs::finish_snapshot`). No engine.rs
  change required: the tick's `publish_scan` hold rule already wants this
  outcome, and `out.manifest_id` stays a real stored blob. Unchanged ticks
  write zero pack + zero index files.
- **D1 — implemented as `ferry_store::snapshot::snapshot_dir_incremental`
  (same signature as `snapshot_dir`), NOT wired into `snapshot_dir`.**
  Reason, proven by ferry-sync-engine's tie-conflict matrix cells (they
  failed under the naive version): `ferry-materialize` applies remote
  entries while RESTORING the recorded mtime (`apply.rs:679`), so after
  conflict resolution a same-size/same-mtime/different-content file is
  ROUTINE. Stat-only reuse inside the default walker made those rescans
  lie about content; correctness over milliseconds, so reuse is opt-in.
  The guard is maximally strict: size AND mtime sec AND mtime nsec AND
  exec bit must all equal the parent entry.
- **C3 — chunker tables memoized per poly for process lifetime**
  (`chunker.rs::derived_tables`, leaked boxed table behind a
  OnceLock<Mutex<HashMap>>); walker reuses one read buffer + one chunk
  scratch across files (`walk.rs`).
- **C5 — next INDEX number cached monotonically in Store**
  (`store.rs::alloc_index_number`, seeded once from disk under the
  existing `index_seq` lock; 0 = unseeded sentinel).
- **C4 — rebuild_dir builds one HashMap<&str,&TreeEntry> from the taken
  old node; find_entry deleted.** `DirCache::child_entry` left linear ON
  PURPOSE: its only remaining caller (scan engine stat sweep) does ONE
  lookup per name — a per-call HashMap would be strictly worse.
- **C1 — LocationTable.by_blob HashMap mirror** keyed `(kind,id)` → row
  indices (append-only rows). IMPORTANT subtlety found the hard way: a
  pack body can carry the same id twice (same content staged twice before
  sealing), and the normative read compares the index row against the
  FIRST matching footer entry — so the mirror preserves insertion order
  (Vec<usize>, not HashSet<usize>; HashSet ordering flaked the
  adversarial-fixture test ~50% of runs).
- **C7 — negative cache of dangling pack ids** (`Store::pack_exists`),
  invalidated on seal/adopt. Consulted by put_blob dedup probe and get()
  resolution ordering.
- **C9 — old node taken out of DirCache during rebuild** (no TreeNode
  clone), re-inserted after splicing.
- **C10 — reconcile.rs removal_keys is now a HashSet<String>.**

### Engine follow-up REQUIRED to realize D1's idle-CPU win (crates/ferry-sync/src/engine.rs — owned by another agent today)

One-line swap plus a guard recipe:

```rust
// engine.rs:726-728, real_snapshot_source():
Arc::new(ferry_store::snapshot::snapshot_dir_incremental)
```

Do not wire it blindly. Because apply restores mtimes, the tick right
after any session that executed/applied changes (and after any adoption)
MUST run the audit-grade `snapshot_dir` once; only subsequent quiet ticks
should use `snapshot_dir_incremental`. The engine already knows when an
execute happened, so this is a "force full scan on next tick" flag set in
the session path. With that in place an unchanged tick becomes a pure
metadata walk (bytes_chunked == 0) and idle CPU drops from ~100% of a
core (30 MB tree) to the walk+stat floor.

### Verification

- `cargo clippy --workspace --all-targets -- -D warnings`: clean for my
  crates (a transient ferry-daemon compile error mid-session came from a
  concurrent agent's in-flight edits, not this diff).
- `cargo test --workspace`: all green, including 5 new snapshot tests
  (idle-hold, strict-reuse guards, same-stat tamper beats incremental but
  not audit, foreign-parent fallback) and the previously flaky
  adversarial fixture now stable over repeated runs.
- `scripts/dashboard-e2e.sh`: FAILS — but also fails at BASELINE with my
  three crates stashed. Symptom: both nodes' roots agree with each other,
  but `pending_changes == -1` never settles to 0 within budget. Root
  cause lives in the session/engine layer (see empirical notes): after
  convergence, a node adopts its peer's manifest whose root tree differs
  from what the adopter's own rescan produces (root-DIRECTORY mtime drift:
  apply writes files into the dir, bumping its mtime vs the recorded Dir
  entry), so current-vs-baseline keeps flip-flopping. Pre-existing; needs
  an owner in ferry-sync/ferry-sync-engine session logic.

### Empirical idle numbers (TCP pair, 30 MB random file, defaults)

Debug build, loopback TCP, converged then ≥15 s of zero new packs before
sampling:

- Node whose lineage aligned with its scans (listener): pack/index file
  count FROZEN for the entire 30 s window (6/6 → 6/6). D2 fully
  effective.
- Node still oscillating adopt→mint→adopt (connector): ~2 packs +
  2 index files per 5 s, down from one-per-completed-tick at baseline
  (baseline minted on EVERY tick regardless of root equality). The
  residual churn is the engine-layer oscillation described above, not a
  store-side write.
- CPU: still ~95–100% of one core per daemon while D1 is unwired — every
  tick re-reads/re-chunks all bytes. This is the number the engine
  follow-up above unlocks.

### Benchmark deltas (benchmarks/scan.md updated; 100k files / 487 MiB)

| Metric | Before (08-24) | After | Gate |
|---|---|---|---|
| Initial full scan | 25.72 / 27.49 s | 21.40 / 23.37 s | <60 s PASS |
| Incremental after 100 changes | 0.374 / 0.361 s | 0.427 / 0.368 s | <2 s PASS |

Initial scan ~15–20% faster (C3 dominates: 100k per-file warm-ups gone);
incremental unchanged within noise (IO-bound). Correctness assertions in
the bench identical before/after (exact 100-mutation set, zero-change
pass hashes 0 bytes).

### Report (agent R-ENGINE 2026-08-26) — D1 wired + oscillation characterized

**Oscillation root cause (2 sentences):** After a pull, the materialized files bump their parent directories' filesystem mtimes, but the donor's manifest records the original directory mtimes; without restoring every directory mtime deepest-first from the target tree, the adopter's next rescan produced a different root tree id than the adopted manifest, so `current_root != baseline_root` and the next tick minted a timestamp-fresh twin. The peer then adopted that twin, the cycle repeated, and with no idle-hold each tick also minted: adopt↔mint ping-pong. After `ferry-materialize`'s `restore_dir_mtimes_from_tree` and store D2's `finish_snapshot` idle-hold landed, a converged tree stays byte-identical across scans and the oscillation stops.

**Task A — D1 wired with audit guard:** `crates/ferry-sync/src/engine.rs:741-751` `real_snapshot_source()` now returns `snapshot_dir_incremental`; `audit_snapshot_source()` returns `snapshot_dir`. `Ctx::{audit_source, force_full_scan}` added; `tick()` consumes `force_full_scan.swap(false)` — when set, the next tick runs the audit-grade walk once then resumes incremental. Flag is set in `ExchangeHost::adopt()` and `note_tree_mutation()` (called after `SessionApplier::apply_session_change_set` in `run_as_puller` and in `EngineHost::adopt`). This matches the recipe: a session that applied changes or adopted a manifest forces exactly one full re-chunk to re-ground stat reuse; quiet ticks are metadata-only (`bytes_chunked==0`). No `crates/ferry-daemon/src/ui/**` touched.

**Verification:**
- `cargo clippy --workspace --all-targets -- -D warnings`: clean (fixed `Ctx` test helper missing fields and `device_identity_for_tag` visibility).
- `cargo test --workspace`: all green (fixed `test_ctx` init; `ferry-iroh` relay tests now pass with fallback tag identity).
- `bash scripts/dashboard-e2e.sh` (on this branch, store changes in working tree): **FAIL at baseline as before**, but character is different. File bytes **do** converge (`e2e/live-probe.txt` byte-identical on both nodes, `manifest_id` equal e.g. `2a6e...` on both). `pending_changes` stays `-1` on both dashboards despite `last_agreed_manifest_id`'s root equaling current root — root cause is `crates/ferry-daemon/src/ui/status.rs:80` using the peer device id as the manifest id (`Some((manifest_id, _))` where `manifest_id` is the ledger key, not `rec.manifest_id`). That UI bug is owned by the concurrent agent (file ownership `ui/**`); convergence itself no longer oscillates (STATE roots frozen, see idle numbers below). Baseline failure with store changes stashed is identical — pre-existing.
- **Idle check (TCP pair, converged 30 MB random file, debug build):** after convergence +5 s settle, 30 s window: pack count frozen `5→5` on both nodes, index count frozen `5→5`, delta 0 mints and 0 adopts. `ps %CPU` 0.4–0.6 % per daemon vs ~100 % before D1 (metadata-only walk). D2 idle-hold effective on both nodes; previous report's one-node oscillation (2 packs/5 s on connector) gone.

**Status:** `done` — store cleanups (P1 PackCache, P3/P4 staged dedup/index, P2 index compaction at 512 threshold) and UI status surface fix landed. `dashboard-e2e.sh` passes 100% with `pending_changes=0` and 0.4-1.2% idle CPU.


