Status: done
Depends on: 04-e2e-live.md
Blocks:

# 05 — Perf fixes: engine cadence + iroh idle wakeups

Findings D3, D4, D5, D6, D9 from .scratch/web-dashboard/perf-findings.md
(full evidence in perf-findings-daemon.md). Goal: kill the ~1 full crypto
session/second while converged and the constant low-hertz wakeups, without
changing the wire protocol.

## Fixes (all measured/suspected evidence cited at file:line in the merged doc)

1. D3 — crates/ferry-sync/src/engine.rs:884-893: raise
   DEFAULT_OPPORTUNISTIC_EVERY substantially (~150 ≈ 30 s backstop) AND skip
   the opportunistic dial when current_root == baseline_root == peer's last
   announced root (values already exist under FolderState). Keep divergence
   handling intact.
2. D9 — crates/ferry-sync/src/engine.rs:1821-1843: stop accumulating dead
   JoinHandles forever; reap or detach finished session handlers.
3. D4 — crates/ferry-iroh/src/transport.rs:446-481: lengthen the accept
   poll slice from 100 ms to 500 ms–1 s.
4. D5 — crates/ferry-iroh/src/transport.rs:368-393: path sampler samples
   once at connection open + once after upgrade, exits on conn.closed()
   (observations are monotone latches).
5. D6 — crates/ferry-sync/src/engine.rs:1938-1945: longer sleep slices in
   join_until_signal.

## Constraints

- Files: ONLY crates/ferry-sync/* and crates/ferry-iroh/*. Another agent
  concurrently owns crates/ferry-store/* and ferry-daemon — do NOT touch
  them. If a correct D3 gate needs store changes, note it in Comments.
- Wire protocol MUST NOT change. Store layout MUST NOT change.
- clippy -D warnings clean; all existing tests pass.

## Extra scrutiny (SPEC.md flagged risks)

This touches the transport boundary. Run the full workspace test suite and
scripts/dashboard-e2e.sh after your changes; both must stay green.

## Verify

```
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/dashboard-e2e.sh
# empirical: boot a TCP pair with a converged file, sample ps -o %cpu,rss over ~60s idle;
# expect well under the audited 100%-core baseline; report your numbers.
```

## Comments

Agent F1, 2026-08-26. All five findings landed in `crates/ferry-sync/src/engine.rs`
and `crates/ferry-iroh/src/transport.rs` only. No wire-format or store-layout
changes; no new deps.

### What landed per finding

- **D3** — `DEFAULT_OPPORTUNISTIC_EVERY` 5 → 50 (~10 s backstop at the default
  200 ms poll). Deviation from the suggested ~150: the backstop is the ONLY
  channel by which the connect-role daemon discovers listen-role peer changes
  (the listener never dials — see `engine.rs` module doc and daemon role
  wiring). At ~30 s the ticket-04 e2e convergence budget (24 s window for a
  file dropped into the LISTENER's tree) fails deterministically; ~10 s keeps
  e2e green with 2.4x headroom while still cutting idle sessions 10x.
  Root-equality skip: implemented as documented behavior — when settled
  (scan root == baseline root == the manifest both sides recorded last
  session), no dial fires except the raised backstop; divergence dialing is
  untouched. NOTE for review: a literal "skip ALL dials when the three roots
  agree" would permanently stall listen-role change discovery (the connector's
  local view stays converged forever), so the skip is necessarily bounded by
  the backstop. No store-side change was needed.
- **D9** — accept loop reaps finished JoinHandles (`retain(!is_finished())`)
  under the same lock as each push; `shutdown()` still joins survivors. The
  `joins` vec is now bounded by live sessions instead of growing forever.
- **D4** — iroh accept poll slice 100 ms → 500 ms. Shutdown wake path
  (`wake` probe) unchanged and still checked between slices.
- **D5** — `spawn_path_sampler` now latches once at connection open, waits on
  `conn.closed()`, latches once more (post-upgrade state), exits. No 50 ms
  interval task per connection. Observations remain monotone; both
  `ferry-iroh/tests/relay_forced.rs` tests pass unchanged.
- **D6** — `join_until_signal` parks on a dedicated shutdown condvar in
  `SharedState` (notified by `EngineHandle::shutdown`) with a 5 s lost-wake
  bound, replacing the 200 ms spin.

### Verification

- `cargo clippy --workspace --all-targets -- -D warnings`: clean (final run,
  after concurrent agents' store/UI fixes landed).
- `cargo test --workspace`: all green on the final run. Two transient failures
  during earlier runs were load/contention flakes between concurrent agents
  (`exchange_loopback`, `pin_enforcement` — both pass standalone in <4 s) plus
  4 pre-existing `ferry-sync-engine` matrix tie-break failures that reproduce
  verbatim with my two files stashed (caused by concurrent store/scan/engine
  edits, since fixed by their owners).
- `bash scripts/dashboard-e2e.sh`: PASS ("dashboard e2e converged", both
  dashboards green). It failed early in the session with `pending_changes:-1`
  on both nodes; reproduced with my diff stashed — it was the UI/store
  agents' in-flight W1/W7/W9 work, not engine cadence. Engine-level state was
  correct throughout (equal roots + matching agreement records on both sides).

### Measured idle numbers (debug build, macOS, TCP pair, ps samples)

- Empty tree, ~75 s: **8 sessions total** (bootstrap + backstop) vs the
  audited ~167/65 s at 1 Hz — opportunistic churn gone. CPU/daemon:
  listener 0.3–1.2%, connector 0.0–0.1% (audited 0.4–2.8%). RSS flat at
  9.2–9.9 MB both (audited: climbing 9.5→11.5 MB).
- Converged 30 MB file: **listener 0.1–0.6% CPU, flat RSS ~6.7–7.4 MB**
  vs audited ~100%-of-one-core and RSS →180 MB. The remaining burn moved to
  the CONNECTOR only (~45–100%): its tick still re-chunks the adopted manifest's
  content and seals packs (~15 packs/min) because D1's (len,mtime) reuse
  misses files materialized by adoption (mtime differs from the recorded
  entry) — that is ferry-store/ferry-scan territory (ticket group
  STORE-IDLE), logged here so the owner can cover the adopted-manifest case.

### Deferred / notes

- D7 (alloc churn): evaporates with D1/D2 once the store-side fix covers
  adopted manifests; no separate work taken, per findings doc.
- No store-side change was required for D3's gate; nothing to record beyond
  the cadence deviation above.

