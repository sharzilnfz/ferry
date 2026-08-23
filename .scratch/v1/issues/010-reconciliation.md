# T-010: Three-way reconciliation and conflict quarantine

Status: done
Depends on: T-003, T-005

Track last-agreed manifest per folder per peer. Reconciliation = three-way
merge between local tree, remote manifest, and last-agreed base (Mutagen's
model). Divergent paths quarantine per ADR-0004: winner stays live, loser
becomes `path.ferry-conflict.<device>-<ts>`, structured report entry.
Deletion-vs-edit conflicts resurrect with a marker.

Acceptance: exhaustive test matrix (both-change, delete-vs-edit, add-vs-add
same content, add-vs-add different content) across two simulated devices;
zero silent data loss in any scenario; `ferry conflicts list` shows every
quarantine.

## Comments

Landed as `crates/ferry-sync-engine` (no async). Public API:
`reconcile(ReconcileInput) -> ActionPlan`, `execute(store, root, plan,
state_dir, now)`, `PeerState` + `AgreedRecord` (last-agreed pointers),
`list_conflicts`/`append_entries` (report), plus `plan`/`naming` types.
The planner is pure: manifests in, plan out; all mutation goes through
ferry-materialize's applier under `Overwrite::Expect { expected: local }`,
so a tampered live file surfaces `Diverged` before anything is touched
(the executor also pre-verifies local-loser bytes region-by-region before
writing any quarantine copy).

Matrix: 32 cells — scenario (both-change / delete-vs-edit /
add-vs-add-same / add-vs-add-diff) × direction (A favored / B favored /
tie) × order of application (A first / B first / simultaneous), plus a few
extra tie-order cells beyond the required 24. Every cell asserts: byte-level
zero-loss on BOTH devices (union of live+quarantined files ⊇ every content
version that existed anywhere pre-reconcile), winner bytes live at the
conflict path on both sides, quarantine copies hold loser bytes under the
loser's name, JSONL parses line-by-line with correct kinds/devices, manifests
reach a fixed point (equal root tree ids) within 6 rounds, one more cycle is
zero ops, and post-convergence agreement pointers record and reload.

Decisions worth knowing:

- Tiebreak: newer entry mtime wins; exact ties go to the higher manifest
  device id (full 32-byte compare). The comparison is symmetric, so both
  devices always pick the same winner. Verified by mirrored-view tests.
- Quarantine naming: `<path>.ferry-conflict.<loser-device-8hex>-<YYYYMMDD-
  HHMMSS>`. The tag is the LOSING device's short id (Syncthing's convention:
  names record where the losing copy came from); the timestamp is the loser
  entry's own mtime UTC, not a wall clock, so both devices derive identical
  names and copies converge byte- and metadata-identically. Collisions
  append `-2`, `-3`, ... to the full name; no extension splitting.
- Both devices quarantine the SAME loser under the SAME name regardless of
  which side executes, so convergence leaves exactly one quarantine copy per
  conflicting path per device. Quarantine files are ordinary files and sync;
  after the next exchange each device also holds the other's copy of shared
  state. Delete-vs-edit writes no quarantine file (a deletion has no bytes)
  but still logs an entry with `quarantined_as: null`.
- JSONL schema (one object per line at `.ferry/conflicts.jsonl`):
  `{ts, folder_id, path, kind, winner:{device,mtime_sec,mtime_nsec},
  loser:{device,mtime_nsec,...}, quarantined_as}`. `kind` ∈ `both_changed`
  | `delete_vs_edit` | `add_vs_add`; loser mtimes are null for deletions;
  ts is RFC3339 UTC. Corrupt lines fail loudly with line numbers.
- Last-agreed state: one file per peer at `<state_dir>/peers/<64hex>.agreed`,
  exactly one canonical 77-byte v1 record each (spec serialization from
  docs/store-format.md). Absent file = initial sync; wrong length, trailing
  bytes, nonzero flags, or mismatched peer id = loud corrupt error.
- Metadata-only divergence (same bytes, differing mtime/exec) resolves
  silently via the tiebreak rules — no conflict file, nothing at risk.
  Deletion never beats ANY difference from base, so even a metadata touch
  resurrects.
- Remote-won deletions that would swallow a locally-winning descendant are
  suppressed; the manifest exchange carries the resurrection instead.
  Resurrections landing inside locally-deleted directories synthesize mkdirs
  from base state. Cross-level structural fights (ancestor retyped while a
  descendant changes) abort loudly with `StructuralConflict` rather than
  guessing; quarantining whole directory subtrees stays out of v1.

One cross-ticket fix this work uncovered: ferry-materialize's planner used
to skip a rewrite when size+exec+mtime all matched live, which silently
drops equal-length divergent edits sharing a timestamp — precisely the tie
case. It now proves content against the store before skipping (apply.rs
plan_upsert). Full workspace suite stays green, including T-005's kill-9
proofs.

Counts: 26 unit tests in-crate + 32 matrix cells (each running multi-round
two-device exchanges), full workspace `cargo test --workspace` green,
clippy --all-targets clean, fmt applied.
