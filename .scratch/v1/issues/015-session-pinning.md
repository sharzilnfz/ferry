# T-015: Session pinning prototype (M4)

Status: done
Depends on: T-010, T-013

`ferry pin start|stop`: while pinned, a device declares active-writer status;
competing remote edits to paths the local tree changed since pin are held and
surfaced (`ferry status` shows held set) instead of racing. Prototype scope:
single peer, manual release. This is the agent-writes-overnight story from
research archetype 7 — the feature no competitor has.

Acceptance: scripted scenario where device A pins, mutates files, device B
mutates the same paths concurrently; B's changes hold until release; release
produces explicit conflicts per ADR-0004, never torn writes.

## Comments

**Found state (resumed session).** A prior worker had scaffolded
`crates/ferry-pin` (Cargo.toml + 6 source files, ~1560 lines) untracked and
uncompilable: one type error in `split.rs`, an unused `state_dir` param in
`plan_release`, and 12 broken tests (stub manifest with a nonexistent root
tree, wrong bare-glob assumption in the matcher test, `Store::create` called
without its parent dir). All fixed; no design changes were needed — the
module layout was sound. Stray root `.DS_Store` deleted, never committed.

**Held-set format.** One JSONL line per held decision at
`.ferry/held/<peer-hex>.jsonl`: `{held_sec, held_nsec, path, device_id,
remote_manifest_id, chunks:[{id,len}], decision:
remote_apply|remote_delete|conflict, conflict_winner}`. Append-only via one
write+sync per batch; readers tolerate exactly one torn final line (crash
mid-append), anything else is loud `LedgerCorrupt`. Cleared per peer only
after that peer's release plan executed successfully.

**Release semantics.** `plan_release` is pure: per peer it takes the LATEST
held entry's manifest as the remote side, the pin-start `base_agreements`
entry as the three-way base (empty ancestor if never agreed), and runs the
ordinary `ferry_sync_engine::reconcile`. The CALLER executes the returned
plan through `ferry_sync_engine::execute` with the state dir, so outcomes
are exactly ADR-0004 outcomes (winner live, loser quarantined
`path.ferry-conflict.<device>-<ts>`, conflicts.jsonl entry). Ledger clear +
`mark_released` happen after execution, so a failed release is retryable and
a second release is a verified no-op. During the hold, fetch stays full
(held bytes land locally so release works offline) while pinned-path-only
chunks are withheld from send.

**Daemon seam status.** Integrated NOW, not deferred. T-013's exchange loop
lives in ferry-cli (`src/exchange.rs::run_round`) and is CLI-owned, so the
seam cost zero changes to ferry-sync / ferry-sync-engine internals:
`ferry_pin::hold_filter(...)` is consulted after the data phase, before
execute; a `Hold` decision appends the ledger batch and executes only the
apply half; `RoundReport`/daemon NDJSON/`ferry sync` output gained a `held`
count. Structural safety: splits that would move ancestor/descendant halves
apart are refused loudly (`StructuralSplit`).

**Pin record.** `.ferry/pin-state.json` v1: `{format_version, device_id,
pid, started_sec/nsec, paths[], released, base_agreements{peer→manifest}}`.
Stale = recorded pid no longer alive (`kill(pid,0)`; EPERM counts alive;
pid 0 degrades active). Stale pins hold nothing but stay on disk and
surface in every status surface until replaced by `start` or discarded by
`stop`; `stop` never discards ledgers.

**CLI.** `ferry pin start [--paths <glob>...] [folder]` (bare start =
whole-folder `*`), `stop`, `release`, `status`, all with the global --json;
typed errors mapped to stable codes (`pin-active`, `bad-pattern`,
`pin-state-corrupt`, `held-ledger-corrupt`, `held-manifest-missing`,
`structural-split`). Schemas documented in docs/cli-json.md and pinned by
blessed snapshots (`tests/expected/pin-*.schema.txt`). `ferry status` gained
`pin` + `held_changes` + `held_by_peer` fields (append-only schema change).

**Verification.** Acceptance scenario in
`crates/ferry-pin/tests/pin_scenario.rs` mirrors T-010's matrix harness:
A pins `src/**`, mutates 3 files; B mutates the same 3 (winning one via
mtime) plus 2 disjoint docs paths → exactly 3 held entries, A's bytes stay
live on all pinned paths, disjoint B changes apply immediately, release
produces 3 both_changed conflicts with both winner directions exercised,
loser copies byte-verified in quarantine, zero-loss check proves every
pre-release version from BOTH trees survives on A afterwards, second
release is a no-op. Plus stale-pin surfacing test, glob edge coverage
(overlap union, non-matching bypass, anchored vs bare names), and 12 new
CLI tests (lifecycle with JSON shapes, bad-glob refusal leaves no marker,
stale replacement recovery, status-held-set). Workspace: 482 tests green,
clippy --all-targets zero warnings, fmt clean.

