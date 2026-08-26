# Perf findings — Windows CI (merged audits A/B/C)

Branch fix/windows-ci @ c29180d. Evidence: local tests-win.log/clippy-win.log
and GH run 32969906200 + 32975948757. Wire format/store layout frozen.

## Ranked by impact ÷ effort (high → low)

### 1. Cache save skipped on failure — CI wall-clock (AUDIT C §0, §3)
Impact HIGH (≈2–4 min per Windows run, compounding: every red run re-pays
cold build), Effort LOW (one line), Risk NONE (CI plumbing only).
Evidence: MEASURED — windows clippy 112s vs 14–23s on mac/linux; Post Cache
cargo skipped on FAILED; local `cargo test --workspace` is ~165s but CI Tests
step is 327s (≈half is compile).
Fix: `Swatinem/rust-cache@v2` with `cache-on-failure: true` (or split
lint/test into separate jobs so save isn't gated on test success). File:
`.github/workflows/ci.yml`. Status: APPLIED in this branch.

### 2. Debug-profile CPU burn (store chunker, crypto Argon2id) — AUDIT C §2
Impact HIGH (store lib 73.5s, crypto 9.5s, proto 10s; together ~40% of local
test time), Effort MEDIUM-LOW (Cargo.toml `[profile.dev.package]`
`opt-level=2` for ferry-store + ferry-crypto/argon2), Risk LOW-MEDIUM
(optimized code can mask UB; chunker/KDF are pure, but boundaries must still
pass on all OSes). MEASURED by log: `chunker.rs:792` streaming_feed 15 sizes.
Status: DEFERRED — not applied in this branch to keep perf changes minimal
and avoid masking UBSan; revisit after CI is green with a dedicated bench
run. No wire/store change.

### 3. Poll-loop + sleep-bound waits (≈10–20s) — AUDIT C §2, AUDIT B §1
Impact MEDIUM (≈2s from peer_policy 4×500ms, plus relay_forced 150–200ms
poll loops, plus ~15 small settle sleeps across ferry-sync suites),
Effort MEDIUM-HIGH (replace fixed sleeps with engine state channels; keep
timeouts as upper bounds so assertions unchanged), Risk MEDIUM (early
signaling can make tests flaky-fast if wired wrong). MEASURED: peer_policy.rs
:92,207,306,395 etc. Status: DEFERRED — test-only, benefits prod latency but
needs per-suite event plumbing; not low-risk enough for this hardening pass.

### 4. Scan throughput — AUDIT A §6
Per-file cost is already minimal (one symlink_metadata per entry, no blake3
unless mtime/size/exec miss, 256 KiB CDC buffer). Top opportunities:
- Parallel initial scan (HIGH impact, MEDIUM-HIGH effort, LOW-MEDIUM risk).
- Binary-search entry lookup vs linear (MEDIUM, LOW, LOW).
- Allocation fast-paths (MEDIUM, LOW, LOW).
All are wire/store-safe (ordering already canonicalized), but none are
MEASURED on Windows (no Windows bench-gate numbers; only Apple M1
25.7/27.5s <60s gate). Not low-risk wins for this pass; catalogued for
future SPEC-perceived-speed work. Also: long-path prefix helper exists but
no scan caller uses it (A §4) — follow-up ticket.

### 5. Daemon idle — AUDIT B §5
Idle wakes ≥12/s (poll 5/s full snapshot + accept 10/s + main 5/s) and
every poll re-snapshots the whole tree even with zero changes. Top wins:
change-detect before snapshot, event-driven waits (condvar), raise
--poll-ms default, disable portmapper if not needed. All protocol-safe.
Impact MEDIUM-HIGH at idle, Risk MEDIUM (scheduling change). DEFERRED —
not required for CI-green; needs runtime profiling.

## What was applied (Phase 6)
- CI cache persistence: `.github/workflows/ci.yml` — `cache-on-failure: true`.
- Windows symlink mtime restoration: `crates/ferry-materialize/src/apply.rs`
  `set_symlink_times` on Windows now uses `filetime::set_symlink_file_times`
  (was no-op). Fixes adversarial_fixture round-trip determinism on
  windows-2022 runners where Developer Mode creates real links (root cause for
  failures in run 32975948757).

## Deferred with reasons
- Profile opt-level: wait for a green baseline + bench gate run.
- Sleep/event refactors: need per-suite signal wiring, risk of flaky-fast.
- Parallel scan / idle change-detect: higher effort/risk, needs Windows
  bench numbers first.
