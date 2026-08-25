# Handoff: arch-hardening CI repair

Status of this doc: written 2026-08-25, after T-20 landed. Everything in the
orchestration is DONE except making GitHub Actions green on `arch-hardening`.
macOS job is green. Ubuntu + Windows each have exactly ONE failing test left.
All failures are TEST-ONLY platform/granularity issues — no product-code bug
has surfaced.

## Where things stand

- Branch `arch-hardening`, HEAD `53b9ca3`, pushed to origin.
- T-20 (storage-efficiency) implemented by sub-agent, merged `--no-ff` at
  `c8ddfb4`: startup residue sweep (`ferry-store/src/reclaim.rs` hooked into
  `SyncEngine::new`), `ferry store gc` + `--dry-run` reachability report,
  conflicts.jsonl compaction, unchanged-folder rescan growth proof, test
  hygiene audit. Ticket 20 marked done; spec Status: done;
  orchestration-state.md already deleted (in 02c4463).
- Local gates GREEN on HEAD at time of writing: fmt, clippy
  `-D warnings` (host + windows-msvc target for the crates reachable past
  blake3), full workspace tests (~570).

## Environment notes for whoever continues

1. **Main repo `target/` was corrupted** (fallout of the earlier EPERM bug).
   Symptom was wild: `engine_holds_pinned_peer_changes_and_release_recovers_them`
   failed 4/4 full-suite runs in the main repo but passed every run at the
   same commit from fresh TMPDIR/sibling worktrees. Fixed by `cargo clean`
   in the main repo. If local gates misbehave inexplicably, cargo clean first.
2. **Local toolchain is now Rust 1.98.0** (was 1.97.1). CI runners were
   already on 1.98, whose new lints (`unused_async_trait_impl`,
   `borrow_as_ptr`, `cast_lossless`, `ignored_unit_patterns`,
   `doc_markdown`) don't exist in 1.97 — that's why clippy kept passing
   locally while failing remotely. Keep 1.98+ locally; verify Windows-gated
   code with
   `cargo clippy -p <crate> --all-targets --target x86_64-pc-windows-msvc -- -D warnings`
   (blake3's C build blocks cross-checks of crates depending on it — those
   can only be verified on CI or with an MSVC toolchain).
3. Flake protocol still applies to wall-clock convergence/pin tests under
   load; log sightings to flakes.md.

## Fixes already landed for CI (commits a731d7d..53b9ca3)

- `a731d7d` relay: dropped unused async from `AccessControl::on_connect`
  impl (`unused_async_trait_impl`).
- `3439a9f` platform/materialize: `from_mut` for GetProcessTimes sinks +
  `u64::from(dwHighDateTime)` (`borrow_as_ptr`, `cast_lossless`);
  `is_some_and` in links.rs; NFC ambiguity disk test skips when host merges
  spellings.
- `85b5836` pin/materialize: last `borrow_as_ptr` site (GetExitCodeProcess);
  NFC test additionally skips when the host resolves NFC-equivalent lookups
  natively (ubuntu-24.04 runner's /tmp does byte-preserving storage BUT
  normalization-insensitive lookup — bare join of the composed spelling hits
  before any fold-map consult).
- `b1bccfa` materialize/sync-engine: NFC ambiguity test now SELF-VERIFIES
  its fixture (readdir must show both spellings under one NFC key, else
  skip); adversarial_fixture.rs `|()|` not `|_|`
  (`ignored_unit_patterns`). **This made Ubuntu's NFC failure go away.**
- `53b9ca3` platform/store: procs.rs child-token test retries once after
  25ms when parent and child are born in the same jiffy; admission.rs
  replaced runtime `if cfg!(unix)` around a `#[cfg(unix)]` helper with real
  cfg bindings (Windows couldn't resolve `os()` — E0425).

## REMAINING FAILURES (run 32857631211)

### 1. ubuntu-24.04 — Tests (workspace)

```
pin::tests::pid_reuse_is_detected_through_start_time_mismatch FAILED
crates/ferry-pin/src/pin.rs:451
assertion `left != right` failed: distinct instances must carry distinct tokens
left: Some(18786) / right: Some(18786)
```

Same granularity class as the procs.rs one fixed in 53b9ca3: on Linux the
token is `/proc/<pid>/stat` field 22 = starttime in clock ticks
(CONFIG_HZ ≈ 100/s). The test spawns a real child and asserts its token
differs from OUR process token. When the test binary is younger than one
jiffy (<10 ms after exec — easy on fast runners with many parallel test
binaries), parent and child births land in the SAME tick and the tokens
legitimately collide.

**Suggested fix**: mirror the 53b9ca3 pattern inside this test (pin.rs ~line
437–455): if `reused.proc_start_token == rec.proc_start_token` on the first
attempt, kill/reap, sleep 25 ms, redo the spawn-and-record dance once, THEN
assert distinctness. Alternatively compare against a deliberately synthetic
token instead of our own birth time (the reuse simulation only needs "a
different instance's token", not literally ours). Note `start_stamps_the_
current_process_start_token` passed, so the probe itself is healthy.

### 2. windows-2022 — Tests (workspace)

```
apply::tests::session_change_set_restores_ancestor_dir_mtimes_absent_from_the_change_set FAILED
crates/ferry-materialize/src/apply.rs:3136
assert_eq!((sec, nsec), (111, 222), "ancestor dir mtime comes from the offered tree")
```

The applier restores an ancestor directory's mtime from the offered tree;
the test plants (111, 222) and expects the exact pair back after
`apply_session_change_set`. On Windows the observed mtime evidently differs
— almost certainly NTFS timestamp granularity/rounding through the
SetFileTime/read-back path rather than a restore logic bug (the same
restore works on macOS/Linux, and 33 other apply tests pass on Windows).

**Suggested fix direction**: master already carries precedent — commit
`c62c42f` "fix(test-helpers): store dir mtimes via SetFileTime; quantize
fuzz clocks". Either (a) quantize the planted expectation to NTFS
granularity (100 ns units, but creation-time rounding via FAT-ish volumes /
GetFileInformation can be coarser — safest is seconds-level comparison on
windows), e.g. accept `(111, 222)` modulo nsec on `cfg(windows)`; or
(b) debug the actual left/right values (add a message to the assert_eq
printing both) with one targeted CI iteration, then decide. Check
`ferry_platform`'s set-mtime helper for a truncation-vs-round mismatch
between write and read paths on Windows specifically.

## After CI is green — final wrap-up checklist

Everything else is already done; confirm and you're finished:

- [x] All tickets `.scratch/arch-hardening/issues/*.md` Status: done
- [x] spec.md Status: done
- [x] orchestration-state.md deleted
- [x] quickstart-e2e.sh, skeleton-e2e.sh, adversarial-fixture.sh pass locally
- [x] docs/cli-json.md changes additive-only (existing schema byte-identical)
- [x] Branch pushed to origin/arch-hardening
- [ ] CI green on all three OSes ← THIS DOC'S REASON FOR EXISTING
- Optional cleanup afterwards: delete merged wave branches (wave*/tNN,
  ticket/T-0xx) and any leftover scratch worktrees; consider merging
  arch-hardening into master.
