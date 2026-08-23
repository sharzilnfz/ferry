# T-005: Materialization

Status: done
Depends on: T-003

Apply a manifest to a tree: write through temp files renamed atomically
(Syncthing temp-name conventions incl. Windows variant), verify every block
hash before rename, handle exec-bit-only permission subset, deletions, and
directory creation order. Never modify a destination file in place.

Acceptance: apply is idempotent (second run is a no-op); kill -9 mid-apply
leaves either old or new state, never a torn file (test with process kill at
randomized points).

## Comments

Done (agent session, branch `ticket/T-005`). New crate `crates/ferry-materialize`
built on `ferry-store`'s public API only (`Store::get`, manifest types,
`diff` shapes, chunker for test oracles). No async; deps beyond the workspace
are none (libc is unix-only dev-dep for SIGKILL, tempfile/blake3/rand already
in-tree).

Public API:

- `Applier::new(&Store, target)` + builders `.overwrite(Overwrite)`,
  `.temp_style(TempStyle)`, `.pace_ms(u64)`; entry points `apply_manifest`,
  `apply_tree(root_tree_id)` (full desired state: creates/updates listed,
  deletes live extras), `apply_change_set(&ChangeSet)` (touches only listed
  paths). Returns `ApplyStats` with mutation counters plus execution-ordered
  `creations`/`deletions` logs — `mutations() == 0` is the idempotence
  assertion.
- `Overwrite::Always` | `Expect { expected: RootManifest }`. Guarded mode
  verifies every path about to be mutated against the base manifest BEFORE
  anything changes (files size/exec/content-verified by streaming against
  store chunks; symlinks by target; dirs by kind; wholesale dir teardowns
  deep-verify their entire live subtree so unaccounted junk blocks deletion).
  Divergences accumulate and return as one `MaterializeError::Diverged`
  listing every offending path+reason, sorted deterministically. Skipped
  (already-correct) paths are never guarded — no mutation means nothing to
  protect.
- `temp`: `TempStyle::{Dot, Windows}`, `temp_name_for`, `hashed_temp_file_name`
  (overflow fallback), `is_temp_name`, `fresh_entropy`,
  `sweep_stale_temps(root, max_age)`.

Decisions:

- Temp names `.ferry.<name>.tmp[.<8hex>]` / Windows-style
  `~ferry~<name>.tmp[.<8hex>]`, created in the DESTINATION directory (same-
  filesystem rename guarantee); entropy tail prevents writer collisions.
  Overflow: if prefix+name+suffix exceeds a conservative 200-byte component
  cap, substitute `<prefix><16 hex of BLAKE3(full rel path)>.tmp` (Syncthing's
  hash-substitution trick). cfg(windows) selects the style on that host;
  both styles are pure functions unit-tested everywhere, and the end-to-end
  apply runs under the Windows style on macOS in one test.
- Verification chain per file: store.get already verifies blob hash after
  decrypt (T-002); applier re-verifies each chunk against its id AND its
  declared length after reading; writes temp; fsyncs; RE-READS every chunk
  region from the temp and re-hashes (pre-rename torn-write coverage);
  sets final mtime+permissions on the temp; renames; fsyncs parent dir.
  Any handled failure removes the temp; the destination was never touched.
- Exec bit authoritative both ways: temp gets 0o755/0o644 pre-rename.
  Non-unix hosts treat exec as uniformly false (documented deviation,
  T-012 territory).
- Idempotence fast path: stat-compare kind/exec/size/mtime; content compare
  only on ambiguity (size+exec match, mtime drift) → mtime-only restore if
  bytes equal, full rewrite otherwise. Symlink mtimes are NOT enforced
  (needs lutimes platform APIs — deferred to T-012); symlinks compare kind+
  target only. Directory mtimes are enforced deepest-first AFTER children.
- Deletions execute children-first (stats log proves order), creations
  parents-first. Change-set minimality: ancestors of written paths must be
  listed or exist; the applier never implicitly touches unlisted parents.
- Stale temps: kept 24h by default (Syncthing resume window;
  `DEFAULT_STALE_TEMP_AGE_SECS`), swept explicitly at startup via
  `sweep_stale_temps`; pattern-conformant files only, never follows symlinks.
- Traversal defense: every stored component rejected unless single, non-empty,
  not `.`/`..`, containing no `/`, `\`, NUL. ENOTDIR during stat (file still
  occupying an ancestor slot mid type-change) is treated as "absent", which
  planning and guard semantics handle correctly.
- Kill-test consistency formalization: whole tree equals old-or-new is
  impossible mid-apply (ops are individually atomic, not transactional), so
  the invariant checked per path: present files re-chunk to exactly the old
  OR new state's chunk-id sequence with matching exec bit; dirs/symlinks must
  exist in and match one of the two states; nothing outside the union exists;
  temps confined to the documented pattern. Brief absence windows are legal
  only inside type-change transitions (unlink→mkdir).

Kill -9 harness (tests/kill_safety.rs + examples/apply_once.rs):

- `apply_once --delay-ms 12` stretches ~30 mutations over ~750ms.
- 25 iterations, seeds `0x5EED_0001 + i` (i = 0..24), SIGKILL via libc at
  uniformly random offsets in [15, 900]ms. Offsets hit this run:
  [526, 377, 317, 425, 470, 153, 668, 750, 455, 597, 187, 717, 439, 564,
  783, 742, 678, 491, 575, 767, 256, 60, 722, 303, 342] (min 60ms, max
  783ms) — kills landed across the entire operation sequence. Every
  iteration verified consistent; any violation would name the seed, offset,
  and divergent path.
- Scenario covers multi-chunk rewrites, exec flips, deletions, added
  subtrees, retargeted symlink, and both directions of type change.
- Harness self-tests prevent a vacuous checker: a no-kill run must equal the
  new state exactly, and a deliberately truncated file must be rejected by
  name.

Tests: 28 new (25 lib + 3 integration; 113 total workspace, all passing;
clippy --all-targets clean; fmt applied). Note for later tickets: run plain
`cargo test` (unfiltered) once so the example binary exists before invoking
the filtered crash tests alone.
