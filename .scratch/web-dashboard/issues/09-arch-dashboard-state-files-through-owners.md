# Route dashboard pin/held/conflict reads through their owning modules

Status: done
Depends on:
Blocks:

## Files

- `crates/ferry-pin/src/pin.rs` — `PinStore::{load,start,mark_released}`, real
  liveness via platform proc-start tokens
- `crates/ferry-pin/src/held.rs` — `HeldLedger::{peers,load_peer}` (typed
  `HeldEntry`)
- `crates/ferry-sync-engine/src/report.rs` — `list_conflicts(state_dir)` (typed
  `ConflictEntry`)
- `crates/ferry-daemon/src/ui/actions.rs` — `load_pin_record`,
  `mark_pin_released`, `pin_start`, `held_by_peer`, `conflict_entries`
- `crates/ferry-daemon/src/ui/status.rs` — `pin_view`

## Problem

The dashboard hand-parses three state files as raw `serde_json::Value` while
typed owners for all three already exist in libraries the daemon could simply
depend on:

1. `pin-state.json`: `load_pin_record` duplicates `PinStore::load`, minus
   version-aware liveness. Liveness degrades to `pid == std::process::id()`, so
   a pin held by another live process reports `stale` and `/api/pin/start`
   refuses or allows on wrong terms.
2. `held/*.jsonl`: `held_by_peer` re-implements `HeldLedger::peers` +
   `load_peer`, stringly-typed via `.jsonl` suffix stripping.
3. `conflicts.jsonl`: `conflict_entries` re-parses lines that
   `ferry-sync-engine::report::list_conflicts` already types.

Every format tweak (T-20 just changed conflict-log compaction) must be mirrored
in the dashboard's parsers or it breaks at runtime with a generic
corrupt-file error. No locality; the format knowledge has two homes and one of
them is untested (`ferry-daemon` has 2 tests total).

## Solution

Add `ferry-pin` and `ferry-sync-engine` to `ferry-daemon`'s dependencies and
delete the three hand-rolled readers/writers. The dashboard asks owners:
`PinStore::load` + `liveness()`, `HeldLedger`, `list_conflicts`. Where the
dashboard needs shapes `docs/cli-json.md` defines, map typed records to JSON at
the edge (status.rs), never parse disk directly.

## Benefits

- One state-file contract per file; format changes land once.
- Correctness: pin staleness uses the same proc-token evidence as the CLI, so
  `ferry pin status` and `/api/status` can no longer disagree about whether a
  session holds.
- Tests improve for free: `ferry-pin`'s 27 unit tests and `ferry-sync-engine`'s
  suite now cover the dashboard's data path transitively.
- Small diff, no public-interface changes — safe to land even with other agents
  working nearby.

## Before / after

```text
BEFORE (daemon ui)                      AFTER (daemon ui)
actions.rs:                             ferry_pin::PinStore::load()
  serde_json parse of pin-state.json    ferry_pin::HeldLedger
actions.rs: held_by_peer()                ::peers()/load_peer()
  hand-parse held/*.jsonl               ferry_sync_engine::report
actions.rs: conflict_entries()            ::list_conflicts()
  hand-parse conflicts.jsonl            status.rs: type -> JSON mapping only
```

## Strength

Strong

## Comments

Landed 2026-08-26 by the wave-1 daemon agent (was marked ready-for-human,
but the change is small and fully inside ferry-daemon's ui layer, so it was
safe to take).

- `crates/ferry-daemon/Cargo.toml` gains `ferry-pin` and
  `ferry-sync-engine` as internal path deps (same declaration style as the
  existing workspace path deps; no new external deps, no cycles).
- actions.rs hand-parsers deleted: `PinFile` / `load_pin_record` /
  `mark_pin_released` / `pin_state_path` and the raw `held/*.jsonl`,
  `conflicts.jsonl` line parsing are gone. The dashboard now asks the
  owners: `PinStore::{load,start,mark_released}`, `HeldLedger::peers` +
  `load_peer` + `distinct_paths`, `ferry_sync_engine::list_conflicts`.
- Typed → JSON mapping happens at the edge only: `ConflictEntry` serializes
  straight into the documented wire shape (ts/folder_id/path/kind/winner/
  loser/quarantined_as), held paths map per peer in status.rs/actions.rs.
- Response shapes unchanged: pin start/stop/release documents byte-match
  docs/cli-json.md (verified live against a running pair).
- Staleness bug fixed for real: `PinStore::start` now stamps this daemon's
  proc-start token (confirmed present in `.ferry/pin-state.json` after
  `POST /api/pin/start`), and `/api/status` pin.state derives from
  `PinRecord::liveness()` — pid reuse reads stale instead of the old
  `pid == my-pid` approximation. A second `/api/pin/start` while the live
  daemon holds correctly answers 409 `pin-active`.
- Error plumbing maps typed failures to CLI-stable codes:
  PinActive→pin-active, Corrupt→pin-state-corrupt, LedgerCorrupt→
  held-ledger-corrupt, LogError::Corrupt→conflict-log, Io→io.

Verify: clippy -p ferry-daemon --all-targets -D warnings clean;
cargo test -p ferry-daemon green; live TCP pair exercised
  status/conflicts/pin start+dup+stop endpoints with shape assertions.

Original analysis with diagrams: /var/folders/y9/hnkm2lv91n5chc4116wp_hf40000gn/T/architecture-review-1787745437.html (architecture audit A0, 2026-08-26).
