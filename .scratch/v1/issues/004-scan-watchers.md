# T-004: Scan pipeline and watchers

Status: done
Depends on: T-003
Blocks: T-010 (reconciliation consumes manifests), T-011 (fills IgnorePolicy)

Incremental scan: fs events (notify crate: FSEvents / inotify /
ReadDirectoryChangesW) plus size/mtime short-circuit, debounced, with
periodic full-hash audit and a poll fallback for Linux descriptor exhaustion
(Mutagen's policy, see research). Must survive inotify queue overflow without
losing correctness (rescan on overflow). Ignore-rule filtering hooks land here
(T-011 fills them).

Benchmark gate: initial scan of a 100k-file / 500 MiB synthetic fixture
under 60 s on CI hardware; incremental rescan after 100 changed files under
2 s. Record numbers in `benchmarks/`.

## Comments

**2026-08-24 — implemented as `crates/ferry-scan`; both gate targets PASS.**

Benchmarks (full methodology + table in `benchmarks/scan.md`), Apple M1 /
macOS 26.6.1 / 8 GiB, release build:

- Initial full scan of 100k files / ~487 MiB: **25.72 s / 27.49 s** (gate < 60 s)
- Incremental rescan after exactly 100 changed files: **0.374 s / 0.361 s**
  (gate < 2 s) — 163 dirty dirs rebuilt, exactly those 100 files rehashed,
  diff vs previous manifest is exactly 100 `content_modified`
- Zero-change pass hashes 0 bytes (`PassStats::bytes_chunked` hook)
- Parallel hashing NOT needed to clear the gate; listed as ordered levers in
  benchmarks/scan.md if a future fixture regresses

Module layout (`crates/ferry-scan`): `policy.rs` (pure overflow/poll
decision layer, unit-tested without kernel timing), `state.rs` (per-dir cache
with BLAKE3 ids for subtree splicing), `walk.rs` (incremental walker:
deepest-first rebuild, size/mtime/exec short-circuit, mirrors snapshot.rs's
NFC/refusal/collision rules), `normalize.rs` (mtime-noise canonical
comparison defining the correctness invariant), `engine.rs` (watcher +
debounced worker + poller + audit threads over std only; `Parts` shared by
worker and synchronous `scan_once()`), `ignore.rs` (`IgnorePolicy` trait +
`NoIgnores`; gitignore syntax deferred to T-011), `config.rs`, `error.rs`.
Binary `bench-gate`. Tests: 25 unit + 6 integration, all green;
`cargo test --workspace` 116 passed; clippy --all-targets clean; fmt applied.

OS policy matrix decisions (documented in crate docs):

- Overflow (inotify Q_OVERFLOW, Windows buffer overrun, FSEvents synthetic
  loss markers) → FULL rescan. Rule: unclassifiable watcher errors are also
  treated as loss — redundant rescans cost seconds, lost events desync peers.
- Linux descriptor exhaustion (ENOSPC/EMFILE/ENFILE from watch registration,
  notify MaxFilesWatch) → mark the subtree unwatchable, start poll fallback
  at `ScanConfig::poll_interval` (default 10 s, Mutagen's interval). Poll
  passes are stat-only sweeps whose mismatches feed the same dirty-subtree
  machinery as native events, so polled state converges to identical
  manifests.
- Root liveness (Mutagen's cheap safety net) checked on every poll tick on
  ALL platforms: root gone → pause; root back → full rescan.
- Debounce hand-rolled (~500 ms quiet window, extends on arrival so bursts
  coalesce into ONE pass) instead of notify-debouncer-full: we need raw event
  paths for the dirty-set computation and wanted the overflow signals
  unmolested; avoids one dependency. Std threads only, no async runtime.

Deviations / deferrals:

- Full scans (initial/audit/overflow recovery) run OUR walker against an
  empty cache rather than ferry-store's `snapshot_dir`: single walking
  codepath so full and incremental scans can never disagree about the
  structurally excluded `.ferry` store dir (snapshot_dir has no exclusion
  rule). Equivalence with snapshot_dir is held by tests on user trees.
  Residual duplication of walk rules between crates noted; revisit if
  snapshot.rs grows features (streaming reads).
- Mid-pass vanishing entries are skipped, not hard-errors like snapshot_dir:
  racing deletions are not a scan bug; next event or audit repairs. Document
  divergence lives in walk.rs module docs.
- Structural `.ferry` exclusion is hard-coded (store-layout contract), NOT an
  IgnorePolicy concern; T-011 adds user-facing rules through the seam.
- macOS-specific: watch root is canonicalized at setup because FSEvents
  reports resolved paths (/private/var...) while callers pass /var... —
  without this every event fails prefix-stripping (caught by integration
  tests).
- Bench numbers are THIS machine (M1). Ticket text says "CI hardware"; when
  cross-platform CI lands (T-012/M3), re-run bench-gate there and append a
  row to benchmarks/scan.md.
