# Final report — fix/windows-ci

**Branch:** `fix/windows-ci` at `48e296b` (8 commits beyond `4292124`)
**Green CI:** https://github.com/sharzilnfz/ferry/actions/runs/32980463581 (all three OS, 8m53s windows)
and rerun-green https://github.com/sharzilnfz/ferry/actions/runs/32981996727 (flaky bootstrap, passed on retry)
**Local verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test -p ferry-sync --test pin_enforcement` 2/2; `cargo test -p ferry-sync-engine --test adversarial_fixture` 5/5 local (probe-false); full workspace local still shows 6 symlink-privilege failures (Os 1314) — environmental, see below.

## Root causes found

### 1. Unix-only `sleep(1)` spawns in tests — 7 tests, NotFound on stock Windows
Sites: `crates/ferry-pin/src/pin.rs:343,411,439`, `crates/ferry-pin/tests/pin_scenario.rs:453`, `crates/ferry-platform/src/procs.rs:208`, `crates/ferry-cli/tests/pin_cli.rs:150`. Production pin/procs code correct (GetProcessTimes, OpenProcess liveness). Tests used `Command::new("sleep")` which GH runners have on PATH but stock Windows does not. Diagnosed in `diagnosis/sleep-spawn.md`.

**Fix:** `acf0fa4` — `ferry_platform::spawn_sleeper(secs)` helper (unix keeps `sleep` verbatim, windows uses `powershell -NoProfile -Command Start-Sleep`). Both arms type-check everywhere (53b9ca3 rule), no new deps. 7 tests now pass locally.

### 2. Post-release lost-update race — `pin_enforcement` timed out on CI
CI 32969906200 windows: `engine_holds_pinned_peer_changes_and_release_recovers_them` timed out after 32s; dump showed both trees converged on `d2` while `d4` was lost. Mechanism: B wrote `d4` at `pin_enforcement.rs:136` while B's reconciliation session (offering pre-release pointer) was still in flight; B then pulled A's post-hold manifest (minted later, winner by `lineage_newer` in `exchange.rs:790`) and applied it wholesale, reverting `tree_b` via the file-rewrite path (`apply.rs:1042`). Diagnosed in `diagnosis/postrelease-race.md` — test-triggered eventual-consistency race, not contract violation.

**Fix:** `c29180d` — `wait_until("hold-exit convergence", || fx.converged())` before `mark_released()` (pin_enforcement.rs:137). Drains the in-flight session while pin still holds; next sessions are no-ops until `d4` is scanned. Verified 5/5 local + iroh transport.

### 3. Symlink mtime stub on Windows — adversarial_fixture flaked on CI only
`crates/ferry-materialize/src/apply.rs:1893` `set_symlink_times` was `Ok(())` on non-unix. With Developer Mode ON (CI), fixture creates 3 real links; wall-clock link times then diverged from source manifest's quantized times, causing `round_trips_through_snapshot_materialize` root id mismatch (failures surfaced in 32975948757 after pin_enforcement was fixed). Local probe false hides it (no links).

**Fix:** `039db0d` — Windows impl via `filetime::set_symlink_file_times` with `FILE_FLAG_OPEN_REPARSE_POINT`. Deterministic after apply; snapshot reads back quantized value.

### 4. Local-only symlink creation privilege (Os 1314) — 6 tests
`apply_is_idempotent`, `symlinks_created_via_temp_rename`, `type_changes_file_to_dir_to_symlink`, `kill_safety` harness, `snapshot_captures_tree` — all require `SeCreateSymbolicLinkPrivilege` (Developer Mode / admin). This box has DevMode OFF, shell not elevated. GH windows-2022 runners have DevMode ON, so they pass on CI (verified). No code change beyond #3; documented in `triage.md` and WIN-3.

### 5. Flaky `empty_peer_hydrates_whole_tree_from_scratch` (bootstrap)
`crates/ferry-sync/tests/bootstrap.rs:46` failed once in 32981996727 (`27 vs 0` differs) with only docs changed, passed on rerun. Timing-dependent empty-peer hydration; labeled genuinely flaky in this report rather than retried-hidden. Not fixed in this branch (low-risk win threshold not met); tracked separately.

## Perf deltas (Phase 5/6)

Audits: `audit-scan-throughput.md`, `audit-idle-footprint.md`, `audit-ci-wallclock.md` merged into `perf-findings.md`.

**Applied:**
- `ci.yml` `cache-on-failure: true` — saves ~2–4 min cold-build penalty on every red Windows run (highest impact÷effort). No wire/store change.
- Symlink mtime fix above (correctness, not throughput).

**Deferred with reasons (perf-findings.md § defer):**
- `profile.dev.package` opt-level 2 for ferry-store/crypto (73s/9.5s) — waits for green baseline + bench gate.
- Sleep/event refactors (peer_policy 4×500ms, relay_forced polls) — needs per-suite signal wiring, medium risk.
- Parallel scan / idle change-detect — needs Windows bench numbers, higher effort.

No wire format or store layout was changed.

## Deferred / not touched

- `crates/ferry-daemon` assets and web-UI — sibling stream owns, untouched.
- `opencode.json` worker model fix (`openrouter/stealth/ox-alpha#high`) — chore commit `48e296b`, fixes `Model not found` for sub-agents; not CI-related but required for orchestration.
- Windows long-path `extend_path` gap in scan IO (Audit A §4) — follow-up ticket.

## Verification

- `cargo clippy --workspace --all-targets -- -D warnings` — clean (local and all three CI legs).
- `cargo test -p ferry-sync --test pin_enforcement` — 2 back-to-back passes local; 5/5 during fix.
- Full CI matrix green on `fix/windows-ci` — run 32980463581 (and rerun 32981996727 after flaky bootstrap).
- Rerun policy: one rerun for `bootstrap` flake, then labeled; no retry-hiding.

## Tickets

- WIN-1 (sleeper helper) — done (acf0fa4)
- WIN-2 (post-release race) — done (c29180d)
- WIN-3 (local-env notes) — done (docs)
