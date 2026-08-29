# Audit C — CI wall-clock, Windows leg

Scope: research only. Evidence: local `tests-win.log` / `clippy-win.log` (repo root,
freshly built) and GH Actions run 32969906200 (branch `fix/windows-ci`), step
timings via jobs API.

## 0. CI step timings (run 32969906200)

| Step (windows-2022) | Dur | macOS-14 | ubuntu-24.04 |
|---|---|---|---|
| Cache cargo (restore) | 1s | 17s | 18s |
| Clippy --workspace --all-targets | **112s** | 14s | 23s |
| Tests (workspace) | **327s** (FAILED) | 168s | 171s |
| Post Cache cargo | **skipped** (job failed) | 27s | 20s |

Two structural findings:

1. Windows clippy is 5–8× slower than the other legs and the cache restore took
   1s — consistent with a **cold cache** for this branch on windows.
2. Because the Tests step failed, `Post Cache cargo` was **skipped**, so no
   cache was saved for Windows at all. Every red Windows run re-pays the full
   cold build (~2–4 min of compile) until a run goes green. This is the single
   largest avoidable cost and it compounds: red → cold → slow → red.

## 1. Per-test-binary durations (Windows, local log `finished in Xs`)

Slowest suites (`tests-win.log` line refs in parens):

| Suite | Time | Nature |
|---|---|---|
| ferry-store lib (107 tests) | 73.53s (:750) | CPU-bound (debug-profile chunking), 1 test >60s |
| ferry-iroh tests/relay_forced.rs | 21.74s (:261) | poll-sleep loops + iroh negotiation |
| ferry-proto tests/acceptance.rs | 10.20s (:559) | real TCP loopback engine pairs |
| ferry-crypto lib (53 tests) | 9.51s (:161) | Argon2id KDF calls in debug profile |
| materialize tests/streaming_apply_memory.rs | 9.51s (:375) | 33 MB apply memory gate (by design) |
| ferry-iroh tests/roundtrip.rs | 9.23s (:272) | QUIC frames incl. dial-timeout budgets |
| ferry-sync-engine tests/matrix.rs | 4.94s (:937) | engine matrix |
| crypto tests/acceptance.rs | 4.71s (:170) | KDF-heavy |
| materialize tests/kill_safety.rs | 4.62s (:367) | FAILED; process kill + pacing |
| ferry-sync tests/peer_policy.rs | 3.44s (:829) | fixed sleeps (see §2) |

Sum of test time locally ≈ 165–175s; CI Tests step is 327s → roughly half of
the Windows Tests step is compile+link of test binaries, not test execution.
CI per-suite breakdown is not extractable from step timings (single step);
local log is the proxy.

## 2. Sleep/time-bound suites (serializable-but-slow)

### Poll loops (could be event/signaled)
- `crates/ferry-iroh/tests/relay_forced.rs:148` — 150 ms/iter convergence poll;
  `:167` — 200 ms/iter marker-landing poll; budgets drive the suite's 21.7 s.
  Purpose: wait for two engines to converge over relay. Replaceable with a
  notified channel from the sync engine's state change (engines are in-process).
- `crates/ferry-sync/tests/convergence.rs:140` — 50 ms/iter wait_converged;
  `:176` — writer thread appends every 5 ms (intentional workload).
- `crates/ferry-cli/tests/exchange_loopback.rs:58,149,169,180,226,266` —
  100–400 ms polls around loopback exchange (suite total 3.13 s).

### Fixed unconditional sleeps (pure waste)
- `crates/ferry-sync/tests/peer_policy.rs:92,207,306,395` — four flat
  `sleep(500ms)` after TOFU/policy setup (purpose: let pin state settle before
  asserting). These alone ≈ 2 s of the suite's 3.44 s; an event/handle from the
  engine's "peer pinned" transition would remove all four.
- `crates/ferry-iroh/tests/relay_forced.rs:194` — flat 300 ms pre-assert settle.
- `crates/ferry-sync/tests/integrity.rs:46`, `incremental_index.rs:37,62`,
  `bootstrap.rs:37`, `pin_enforcement.rs:38,163`, `protocol_v1.rs:480,584`,
  `cli/tests/share_gating.rs:77` — 50–200 ms settle sleeps; each small, ~15 in
  aggregate across the ferry-sync integration suites (~10 s combined).

### Production-code polling inherited by tests
- `crates/ferry-sync/src/engine.rs:1850,1866` — 50 ms + `cfg.poll_interval`
  main-loop sleep; `:1943` 200 ms, `:2405/:2416` 150/250 ms retry backoff.
  Test suites that spawn real engines pay these intervals per scenario.
  An internal Notify/condvar would benefit prod latency too, but changes core
  behavior — highest risk item here.

### NOT sleep-bound (do not "optimize" these with sleeps removed)
- store lib 73.5 s: `chunker.rs:792`
  `streaming_feed_boundaries_are_identical_to_slice_output` — 15 input sizes ×
  block sizes through rolling-hash in **unoptimized** code. Work, not waits.
- crypto lib 9.5 s / acceptance 4.7 s: Argon2id m=19456 KiB t=2 per call
  (`ferry-crypto/src/recovery.rs:16,56-59`) under debug profile.
- streaming_apply_memory 9.5 s: 33 MB gate (`tests/streaming_apply_memory.rs:103`)
  exists to measure allocation — keep as-is.
- proto acceptance 10.2 s: full TCP loopback engine pairs doing real transfers.

## 3. Rebuild / redundancy

- Steps share one target dir within the job (default workspace target),
  `Swatinem/rust-cache@v2` present (`.github/workflows/ci.yml:39`), ordering
  fmt → clippy → test (ci.yml:42-48). No separate `cargo build`.
- However `cargo clippy --all-targets` runs under the **check** profile: it does
  metadata+lint+MIR-check but emits **no object code or linked binaries**. The
  Tests step therefore pays full codegen+link for all deps and every test
  binary regardless of clippy. On Linux/macOS this is masked by warm caches
  (clippy 14–23 s); on Windows cold, clippy costs 112 s and then tests re-pay
  codegen inside their 327 s. Estimated avoidable compile on a warm-cache
  Windows run: ~90–150 s of clippy time shrinks to seconds, and the test-step
  compile portion drops to incremental.
- The real fix is cache *persistence*: today Windows never saves (post step
  skipped on failure). Options: fix the failing tests (see audit B), or split
  lint/test so a test failure doesn't forfeit the cache save.
- Optional visibility win: `cargo test --workspace --no-run` as its own step to
  separate compile from execute timing (no wall-clock saving by itself).
- Defender/AV scanning on windows-2022 inflates both steps; nothing actionable
  in-repo beyond fewer files touched (warm cache helps here too).

## 4. Top-3 ranked reductions

| # | Action | Impact | Effort | Regression risk |
|---|---|---|---|---|
| 1 | Get Windows cache warm & saved: make tests green (or reorder/split so cache save isn't skipped on failure, e.g. run clippy+fmt in a separate job that saves independently). Saves the ~2–4 min cold-build penalty **every** Windows run. | High (≈2–4 min/run, compounding) | Low | None — pure CI plumbing |
| 2 | Optimize debug builds for compute-heavy crates: `[profile.dev.package.ferry-store] opt-level = 2` (or a shared `[profile.test]` bump) plus same for argon2/crypto deps via `[profile.dev.package."*"]` opt-level 2. Store lib 73.5 s → est. <10 s; crypto 9.5 s → est. 2–3 s. Coverage unchanged (same tests run). | High (≈70–80 s local; similar share of CI test step) | Medium-low | Low-medium: optimized code can mask arithmetic UB; chunker/KDF are pure functions, acceptable. Verify boundaries still pass on all OSes. |
| 3 | Replace fixed/poll sleeps with event signaling in peer_policy (4×500 ms fixed, peer_policy.rs:92,207,306,395), relay_forced poll loops (:148,:167,:194), exchange_loopback polls. Est. 10–20 s across Windows leg. | Medium (≈10–20 s) | Medium-high | Medium: event-driven waits can be flaky-fast if signaled too early; keep existing timeout budgets as upper bounds so coverage (assertions) is unchanged. |

Not recommended now: touching engine.rs production poll loop (§2 last bullet) —
largest blast radius for modest test-side gain.

## 5-line summary

1. Windows leg (run 32969906200): Clippy 112s + Tests 327s vs 14–23s / 168–171s elsewhere; ~half the Tests step is compile, not test execution.
2. Failed Tests step skipped Post Cache cargo → Windows never saves a warm cache; every red run re-pays a full cold build (~2–4 min) — biggest avoidable cost.
3. store lib 73.5s and crypto 9.5s are debug-profile CPU work (chunker.rs:792, Argon2id recovery.rs:16), not sleeps — fixed by targeted opt-level=2, no coverage change.
4. Sleep-bound suites: peer_policy 4×500ms flat sleeps (:92,:207,:306,:395), relay_forced poll loops (relay_forced.rs:148,167,194), plus ~15 small settle sleeps across ferry-sync/cli suites — convertible to event waits.
5. Top-3 by impact÷effort: (1) restore+save Windows cache, (2) profile.dev opt-level for ferry-store/crypto deps, (3) event-signal the peer_policy/relay_forced waits — all coverage-preserving.
