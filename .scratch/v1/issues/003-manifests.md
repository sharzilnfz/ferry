# T-003: Manifests and tree snapshots

Status: done
Depends on: T-002

Manifests per the T-001 schema: deterministic serialization, hash-addressed,
tree nodes deduplicated like restic's trees. API: snapshot a directory into a
manifest; diff two manifests into a change set (added/removed/modified paths
with required blob lists). Manifest diff must not touch file bytes.

Acceptance: snapshot → mutate tree → resnapshot → diff shows exactly the
mutations; identical trees produce byte-identical manifests.

## Comments

Done (agent session, branch `ticket/T-003`). Built entirely on the T-002
serializers (`ferry-store::manifest`) and store facade; no new crate, no new
dependencies, zero deviations from `docs/store-format.md`.

Public API added to `crates/ferry-store`:

- `snapshot::snapshot_dir(&Store, poly, source, &SnapshotIdentity) ->
  SnapshotOutput` — walks a real directory bottom-up, chunks file contents
  through the folder chunker into staging packs, stores deduplicated tree
  nodes + the root manifest as metadata blobs, and seals all staging packs
  before returning (end-of-burst rule).
- `diff::diff_manifests` / `diff::diff_roots` → `ChangeSet { added, removed,
  content_modified, metadata_modified, type_changed }`, each entry carrying
  path components (NFC), kind, exec flag, mtime pair, ordered chunk list,
  and symlink target — everything T-005's materializer needs.
- `diff::serialize_change_set` / `parse_change_set` — internal codec for
  tests/logging only; explicitly NOT part of the docs/store-format.md
  contract.

Decisions:

- Unknown file types (sockets/FIFOs/devices), non-UTF-8 names, and non-UTF-8
  symlink targets do not abort a snapshot and are never silently skipped:
  each is recorded in `SnapshotOutput::refused` with its path and reason.
  Callers assert an empty refusal ledger when they want strictness. Hard IO
  failures (vanished/unreadable entries) remain errors with the path
  attached. Sibling names colliding after NFC normalization are a hard error
  (the format forbids duplicate names; merging would lose data).
- Type changes (file↔dir↔symlink) surface once in an explicit
  `type_changed` bucket carrying before/after states, not as remove+add.
- Symlink retarget classifies as ContentModified (the target IS the link's
  content); mtime-only bumps are MetadataModified everywhere.
- A directory whose subtree identity is unchanged is not reported even if
  its own mtime moved: dir mtimes churn with any nested edit, and
  materializers set directory times last anyway.
- Diff ordering contract: every bucket sorted ascending by component vector;
  parents precede children; equal root ids short-circuit to empty and equal
  child ids prune whole subtrees.

Tests: 12 new (85 total workspace, all passing; clippy --all-targets clean,
fmt applied). Coverage includes both verbatim acceptance criteria plus:
diff proven byte-blind by deleting every data pack AND the source tree
before diffing from a reopened store; CDC tail-append stability end-to-end
(all-but-final chunk ids shared as an exact prefix); store-level dedup of
identical sibling directories observable through a reopened index; resnapshot
of unchanged trees writes no new pack bytes; seeded fuzz loop checking
double-snapshot determinism and model-oracle agreement for single mutations.

Note for T-005: `snapshot_dir` flushes staging packs but does not append an
index file; callers that reopen later must call `write_index_snapshot()` as
part of ending their burst (mirrors the T-002 store contract).
