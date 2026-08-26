# Audit A — tree-scan and watch throughput on NTFS (`ferry-scan`)

Research-only. Branch `fix/windows-ci`. All paths repo-relative; `file:line`.

## 1. How the tree walk works today

- **Iteration API**: per dirty directory, `std::fs::read_dir` collects only
  names (crates/ferry-scan/src/walk.rs:255-272), sorted by encoded bytes
  (walk.rs:273). Deepest-first over the ancestor-closed dirty set
  (walk.rs:152-153). Untouched dirs are spliced from cache with **zero**
  stats on their contents (walk.rs:487-507, doc walk.rs:19-20).
- **Stat calls**: exactly one `symlink_metadata` per surviving entry of a
  rebuilt dir (walk.rs:312), done *before* ignore consult so the kind is
  free (walk.rs:307-311). Symlinks add one `read_link` (walk.rs:344).
  Poll sweeps do one stat per file plus an existence check per cached entry
  (crates/ferry-scan/src/engine.rs:945, engine.rs:901-908).
- **Hashing strategy**: no direct blake3-of-file. Files stream through the
  CDC chunker in a bounded 256 KiB buffer, each completed chunk stored as it
  closes (walk.rs:439-483) — peak RSS is one chunk, not file size.
  **Short-circuit** `reusable()` compares prev size + mtime(sec,nsec) + exec;
  hit ⇒ chunks cloned from cache, zero bytes read (walk.rs:382-391,
  walk.rs:525-537).
- **Allocation churn (SUSPECTED hot at scale)**: every entry clones
  `rel` → `child_rel: Vec<String>` twice (walk.rs:303, walk.rs:372);
  `admit_name` NFC-collects a fresh String per name even when already NFC
  (crates/ferry-store/src/admission.rs:112-122); `find_entry` linear-scans
  entries per file (walk.rs:518-520) making a rebuilt dir's short-circuit
  lookup O(n²); `DirCache::child_entry` same (state.rs:43-50);
  `ensure_no_collisions` builds a HashSet plus casefold index per rebuilt dir
  (crates/ferry-store/src/snapshot.rs:229-276).

## 2. mtime short-circuit granularity

- Compared values: `(i64 sec, u32 nsec)` from `Metadata::modified()` via
  `ferry_platform::split_unix` — full sub-second nanos, exact equality
  (crates/ferry-platform/src/time.rs:13-25; walk.rs:530-533).
- On Windows `SystemTime` is FILETIME-backed (100 ns units): sub-100 ns
  digits cannot exist, so stored vs re-read mtimes match bit-exactly —
  **needless rehashing from NTFS quantization is not possible** because both
  sides come from the same clock source (time.rs:41-44 documents NS_GRAN=100;
  README.md:114-117 documents that Windows restores mtimes to the last
  100ns-representable value; quantization lives in materialize's write path,
  crates/ferry-materialize/src/apply.rs:2188-2191, and test helpers,
  ferry-store/src/snapshot.rs:547-548).
- **Missed changes (MEASURED-by-test, by design)**: a same-length rewrite
  with mtime restored is invisible to stat; the scheduled full-hash audit
  repairs it (config.rs:11-14 default 24 h; proof test walk.rs:836-883).
  Coarser clocks (exFAT/FAT 2 s) would widen this window; NTFS 100 ns does
  not make it worse than ext4/APFS beyond the audit design already covering.

## 3. Watch mode

- Uses `notify = "8"` `RecommendedWatcher` (Cargo.toml; engine.rs:34,
  engine.rs:602-646) — on Windows this is the ReadDirectoryChangesW backend —
  recursive at the root (engine.rs:641-643).
- **Debounce/burst**: worker drains the queue, then extends a quiet window
  (default 500 ms, config.rs:23) on each arrival so bursts coalesce into ONE
  pass (engine.rs:663-676). No full-rescan-per-event path exists: normal
  events map to enclosing-dir dirty marks (policy.rs:139-153) and incremental
  splice (engine.rs:285-286).
- **Rescan fallbacks**: any watcher error classified `Loss`, any pathless
  synthetic event, or overflow ⇒ `FullRescan` = whole-tree re-read+rehash
  against empty cache (engine.rs:604-635, engine.rs:834-852;
  policy.rs:99-101; execution engine.rs:308-335). Root return also full-rescans
  (policy.rs:116-118). Poll thread checks root liveness and stat-sweeps only
  unwatchable subtrees every 10 s (engine.rs:692-740, engine.rs:858-913).

## 4. Windows-specific costs

- **Long-path prefix**: `ferry_platform::winpath::{needs_extended_length,
  extend_path}` exist (winpath.rs:45-79) but grep shows **no caller in
  ferry-scan/ferry-store scan IO** — only tests/ferry-materialize reference
  them. SUSPECTED gap: deep trees (>260 chars) will fail scans with loud IO
  errors rather than being walked. Partial mitigation: `canonicalize` at
  watch-root creation returns a `\\?\`-prefixed root on Windows which then
  prefixes all walker paths for free (engine.rs:483-484).
- **Case-fold index**: on folding hosts, every rebuilt dir runs
  `ensure_no_host_case_collisions`; `fold_key` = NFC + Unicode
  `to_lowercase` + ς-fold per name (casefold.rs:38-42; snapshot.rs:253-263).
  Allocation per sibling, O(n) per dir — small but nonzero on Windows where
  `host_folds_case()` is true (casefold.rs:121-128).
- **Reserved-name check**: per entry, ASCII compare of stem against 22
  constants (admission.rs:143; reserved.rs:33-37) — negligible.
- **Per-event cost (SUSPECTED)**: `abs_to_rel` NFC-normalizes every event
  component (engine.rs:795-803); `any_prefix_ignored` re-queries the ignore
  chain per ancestor depth ×2 kinds (engine.rs:813-825), and
  `FerryIgnore::decided` joins components into a String per depth×layer and
  holds an RwLock read/write per overlay lookup (ferry-ignore/src/policy.rs:
  152-208, 213-223) — bursty RDCW event storms pay this repeatedly.

## 5. MEASURED vs SUSPECTED

**MEASURED** (benchmarks/scan.md:21-42, produced by
crates/ferry-scan/src/bin/bench-gate.rs, gates at bench-gate.rs:38-43):
100k files/~487 MiB, Apple M1, release: initial full scan 25.72/27.49 s
(gate <60 s ≈17-19 MiB/s single-threaded); incremental after 100 changed
files 0.374/0.361 s (gate <2 s; 163 dirs rebuilt, ~501 KiB hashed); zero-
change pass hashed 0 bytes. **No Windows numbers exist**: tests-win.log:608
shows bench-gate compiled debug-only on the CI runner; machine_info has no
Windows branch (bench-gate.rs:102-107).

**SUSPECTED** (code reading, unmeasured on NTFS): O(n²) entry lookup in big
dirs; per-entry String/NFC allocations; per-event ignore-chain cost under
RDCW bursts; >260-char paths failing; notify buffer-overrun → FullRescan
amplitude on busy trees; single-threaded hashing as the initial-scan ceiling.

## 6. Top 3 improvement opportunities

1. **Parallelize the initial full scan across files** (std threads; content
   addressing makes ordering trivially recoverable by sorting before
   serialization). Impact HIGH (initial scan is the dominant cost, ~17-19
   MiB/s scalar); effort MEDIUM-HIGH; regression risk LOW-MEDIUM — tree-node
   order is already canonicalized by sort, wire format/store layout
   unchanged. Re-run bench-gate after (scan.md:83-85 mandates this).
2. **Binary-search / HashMap entry lookup instead of linear scans**
   (`find_entry` walk.rs:518-520; `child_entry` state.rs:43-50; entries are
   already sorted by NFC bytes per manifest invariant). Impact MEDIUM (large
   directories, incremental passes rebuild parents); effort LOW; risk LOW —
   pure lookup change, no format change.
3. **Cut per-entry/per-event allocations**: fast-path `admit_name` when
   `is_nfc` (pattern already exists at ferry-ignore/src/policy.rs:160),
   reuse `child_rel` buffers in `rebuild_dir`, cache joined strings in
   `match_layer`. Impact MEDIUM on event-storm CPU, LOW on throughput gates;
   effort LOW; risk LOW. Flagged non-option: anything touching mtime
   representation, chunk boundaries, tree-node serialization, or manifest
   layout is FORBIDDEN (wire format/store layout frozen) — notably do NOT
   "optimize" by truncating mtime to seconds or switching hash strategy.

Also worth a follow-up ticket (outside top 3): route scan IO through
`extend_path` for >260-char safety, and consider demoting ReadDirectoryChangesW
buffer-overrun from FullRescan to root-subtree rescan (correctness-sensitive).
