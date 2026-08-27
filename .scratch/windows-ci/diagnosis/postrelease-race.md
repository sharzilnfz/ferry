# WIN-2 diagnosis: post-release lost update in pin_enforcement (run 32969906200)

Scope: mechanics, winner selection, interleavings, classification, fix sketch.
All paths relative to repo root; `engine.rs` = crates/ferry-sync/src/engine.rs,
`exchange.rs` = crates/ferry-sync/src/exchange.rs, `apply.rs` =
crates/ferry-materialize/src/apply.rs.

## 1. Engine mechanics

- Poll cadence: `DEFAULT_POLL_INTERVAL = 200ms` (engine.rs:56-57); tests
  override to 50ms (`default_for_test`, engine.rs:238). Dedicated thread
  `<tag>-poll` spawns at start (engine.rs:1739-1746) and sleeps
  `poll_interval` between ticks (engine.rs:1856-1868). Confirms
  audit-idle-footprint.md row 1 (`Ctx::tick`, engine.rs:840-894).
- Every tick does a FULL `snapshot_dir` walk+chunk+hash of the working tree
  (snapshot_source injected at engine.rs:727, called at engine.rs:852-853).
  Publication is guarded: `publish_scan` discards the scan if an adoption
  changed the pointer mid-scan (`ScanToken`, engine.rs:405-440). Nothing
  guards the scan against a CONCURRENT APPLY on another thread — sessions
  run on accept/dial threads while the poll thread keeps scanning.
- Tick order matters: scan -> publish -> maybe dial (engine.rs:843-891).
  A session therefore always OFFERS the pointer as of session start
  (`ctx.current_snapshot` reads the `current` pointer, engine.rs:793-798,
  1014, 1021-1025) — which can be arbitrarily staler than the disk if the
  last scan predated a local write. All dials are B-side in this fixture
  (A has `connect_to=None`; dial gated at engine.rs:884) and serialize
  under B's `session_lock` (engine.rs:889).
- What an apply does: `SessionApplier::apply` ->
  `Applier::apply_session_change_set` (applier.rs:113-124; apply.rs:442-452)
  = apply exactly the diff buckets, then restore every DIRECTORY mtime from
  the target tree deepest-first (apply.rs:457-475). Files land via
  temp+rename with final mtime set on the temp BEFORE the rename
  (apply.rs:908-911) — the destination never exists with wrong metadata, so
  a concurrent scanner sees old-state or correct-new-state, never drifted
  state, per file. mtimes ARE restored (files and dirs).
- Rewrite policy: `plan_upsert` skips only if kind+size+bytes+mtime all
  match (apply.rs:1042-1058). Size-or-content mismatch => FULL byte rewrite
  (apply.rs:1042-1049). Equal-length different bytes ("d4" vs "d2") hit the
  content check and rewrite. This is the clobber primitive.

## 2. Winner selection — hypothesis verdict

- Manifest level: `lineage_newer` (exchange.rs:790-806) compares
  `(created_sec, created_nsec, device_id, root_tree_id)` lexicographically;
  gates adopt-vs-skip at exchange.rs:403 (equal roots) and :413 (stale-offer
  guard). Same key in the legacy path (`lineage_winner`, engine.rs:621-625;
  `select_donor`, engine.rs:1220-1231). So competing manifests resolve by
  MANIFEST RECENCY, not content.
- Per-path mtime (`pick_winner`: newer entry mtime, higher-device tiebreak,
  reconcile.rs:114-141) exists ONLY in ferry-sync-engine's three-way
  planner (release planner / CLI cycles; lib.rs:1-49). The live v1 pull
  path NEVER consults it: `pull_content` diffs `cur.manifest` vs the peer
  manifest two-way and materializes the result wholesale
  (exchange.rs:482, 514-522).
- Verdict: hypothesis CONFIRMED at mechanism level. A's post-hold manifest
  M_A3 is minted after A applied d2 during the hold (its created_sec/nsec
  stamps come from `(self.clock)()` at tick time, engine.rs:844, 849-850),
  so it beats B's M_B2 by recency; B pulls it and applies the changeset
  directly onto its tree. Any changeset entry naming docs/other.txt rewrites
  the file to d2 (apply.rs:1042-1049), destroying the unscanned d4 edit.

## 3. Why the window exists

State at pin_enforcement.rs:132: A disk {notes v1, other d2@T_b} (d2 applied
during hold, mtimes restored), A pointer M_A3 (minted on the first tick after
that apply; created AFTER M_B2); B disk {v2, d2@T_b}, B pointer M_B2;
agreement ledger still shows the pre-pin baseline G0 on both sides
(pin_enforcement.rs:66-71), roots differ, so the reconciliation round (B
adopts M_A3, reverts notes v2->v1, records agreement) is still pending or in
flight.

Enumerated interleaving that produces the dump (agreed_a==agreed_b,
tree_b=="d2" despite the synchronous d4 write at pin_enforcement.rs:136):

1. B's poll tick N scans PRE-d4 (publishes Held; pointer stays M_B2) and
   dials, OR B's tick starts the pending reconciliation dial. Session S
   snapshots B's cur = M_B2 at engine.rs:1014.
2. Test runs mark_released (:133) then fs::write(d4) (:136) while S is in
   flight. S's changeset was computed from pointers, so d4 is invisible to it.
3. B pulls A's offered manifest (lineage-newer, exchange.rs:413 gate passes),
   diffs M_B2 vs M_A3 (exchange.rs:482) and applies at :514-522. When the
   changeset names docs/other.txt (see below), the size-equal/content-
   divergent rewrite (apply.rs:1042-1049) lands AFTER the test's write:
   tree_b reverts to "d2". held==0 (B has no pin), so B adopts M_A3 and
   records agreement (exchange.rs:425-426) -> agreed_a==agreed_b.
4. Steady state: B's disk now equals the adopted root, so every later scan
   publishes Held (engine.rs:425-438); opportunistic dials settle
   "converged already" (engine.rs:1179-1185). d4 is never scanned again ->
   tree_a stays d2 -> 30s timeout at pin_enforcement.rs:149.

Narrowest sufficient condition: an apply executing on B AFTER line 136 whose
changeset includes docs/other.txt in ANY bucket, while B's session pointer
predates the write. Notes: content of other.txt is identical on both sides
(same bytes, deterministic chunking), exec is uniformly false on Windows
(snapshot.rs:297-308), and dir-mtime-only drift is deliberately not reported
(diff.rs:15-16) — so the changeset can only include other.txt via FILE-ENTRY
metadata drift (recorded mtime differing between M_B2 and M_A3). The dump
alone cannot prove where the drift originates (candidates: CI-host mtime
granularity asymmetry between fs::write stamping and restore, or a scanner/
applier overlap on A publishing a slightly-off entry); per-file atomicity
(apply.rs:908-911) rules out the half-applied-file explanations. The core
window — recency-winning wholesale apply over an unscanned local edit — is
fully verified; the metadata-drift amplifier is inferred as the only
consistent trigger for other.txt being in the changeset.

## 4. Classification

TEST-TRIGGERED eventual-consistency race, not a contract violation by this
test's own terms:

- README.md:43-45 ("Concurrent edits produce explicit conflict files..."),
  CONTEXT.md:54-58 and ADR-0004 (docs/adr/0004-conflicts-quarantine.md:16-28,
  "No silent data loss is possible") describe the THREE-WAY reconciler's
  contract for two SCANNED, divergent states. They say nothing about writes
  that race an in-flight exchange; no scan-based syncer protects unscanned
  local state, and ferry-sync's model is explicitly manifest-per-poll
  (lib.rs:68: "No watching (200 ms polling...)").
- The engine pull path legitimately applies a recency-winning manifest
  two-way; the test wrote into tree_b while a session could still be
  applying the pre-release reconciliation — the exact window sibling tests
  avoid by asserting only eventual convergence of scanned state
  (convergence.rs:131-146 `wait_converged`; incremental_index.rs:34-37,
  bootstrap.rs:34-37 all gate on `fx.converged() && trees_identical`).
- Residual product sharp edge (follow-up worthy, NOT this fix): because
  metadata_modified entries trigger full rewrites (apply.rs:1042-1049), the
  lost-update window is wider than "peer edited the same content" — a pure
  mtime-record drift makes a no-op look like a change. The narrowest
  production guard without wire/store changes (e.g., applier skipping
  rewrites of locally-hot files) needs unsynced-local-edit knowledge the
  applier does not have, so it is not cheap; file as a separate ticket.

## 5. Minimal fix sketch (test-side quiesce)

Drain the pending reconciliation session BEFORE releasing/writing, using
only existing primitives. Insert before pin_enforcement.rs:133:

```rust
// Drain the post-hold reconciliation round: B must have adopted A's
// post-apply manifest (roots AND agreement equal) so no in-flight or
// later session can still carry a changeset that touches other.txt
// while the test writes d4.
wait_until("hold-exit convergence", || fx.converged());
```

Why this closes the window deterministically:
- `converged()` (common/mod.rs:169-179) is true exactly when pointers AND
  agreement match — i.e., the M_B2-vs-M_A3 reconciliation session has fully
  completed (agreement is recorded last, engine.rs:1204/:1207). The one
  dangerous session can no longer exist.
- Afterwards every session is an equal-root no-op (pull_needed false,
  exchange.rs:202-207; "converged already", engine.rs:1183-1185) until B's
  next tick SCANS d4 and mints a child (engine.rs:861-891), which then
  flows normally. A session dialing between the write and that scan offers
  equal pointers on both sides and cannot rewrite anything.
- Pattern matches sibling tests (gate on real engine state, not sleeps);
  budget stays env-tunable via `timeout_from_env` (common/mod.rs:28-34).
  Do not replace with fixed sleeps (CI-wallclock flaky, see
  .scratch/windows-ci/audit-ci-wallclock.md).

## 6. Regression risk / cfg-gate concerns

- The fix is test-only, platform-neutral: `converged()` and `wait_until`
  are already used cross-platform (bootstrap/incremental/convergence).
  Adds one bounded wait (<=30s default) — worst case slower, not flakier;
  it strictly removes a race, so macOS/Linux legs gain the same immunity.
- No new cfg-gated code, so the 53b9ca3 rule (cfg-gated items must
  type-check on all hosts; prefer `cfg!(unix)` runtime bools over
  `#[cfg(unix)]` when types are referenced ungated — see commit 53b9ca3)
  is trivially satisfied. If the follow-up production ticket touches mtime
  handling, mirror 4292124 (filetime crate, ungated signatures) so Windows
  paths stay type-checked everywhere.
- Run `FERRY_SYNC_E2E_TRANSPORT=iroh cargo test -p ferry-sync` once: the
  seam (common/mod.rs:49-66) keeps scenarios identical, but the extra wait
  should be validated on the iroh path too.
