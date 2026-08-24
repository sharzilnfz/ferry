# T-02: Store boundary hardening — no panics on user/disk/wire input

Status: ready-for-agent

Three panic/DoS paths at ferry-store's trust boundaries:

1. **Chunker panics on user-supplied polynomial** (High). `crates/ferry-store/
src/chunker.rs:284,295`: `chunk_offsets`/`chunk` `.expect()` away
`Chunker::new`'s error; the daemon accepts `--poly HEX16` from the CLI flag
(`crates/ferry-daemon/src/main.rs:197`) so a typo panics mid-scan. Fix: make
both free functions return `Result<_, PolynomialError>`, update call sites
(ferry-scan walk.rs rehash path is the main one), AND introduce a
`ValidatedPoly(u64)` newtype constructed once at folder-open/config-load;
prefer threading the validated type through scan/snapshot APIs so downstream
code cannot hold an invalid poly.

2. **Attacker-influenced allocation** (Medium). `crates/ferry-store/src/
manifest.rs:610-611`: `Vec::with_capacity(chunk_count)` where chunk_count is
a raw wire u32 (frame body capped 64 MiB) — up to ~139 GB reservation before
reading a byte. Fix: never pre-reserve from untrusted counts; grow
incrementally or cap by remaining frame bytes. Audit other `u32()? as usize`
capacities in the same file.

3. **Unsigned underflow on corrupt index trailer** (Medium). `crates/ferry-
store/src/index.rs:404-411`: `tlen_pos - tlen` panics (debug overflow check)
before its own guard runs. Fix: `checked_sub(...).ok_or(Corrupt)` then keep
the lower-bound check.

Also fix the O(n²) `Vec::contains` in `LocationTable::merge`/`packs()`
(index.rs:461,477) with HashSet/BTreeSet — same crate, mechanical.

Acceptance: unit tests feeding a reducible poly, a lying chunk_count, and a
truncated/corrupt index trailer all return typed errors (no panic, debug +
release); existing store tests green.
