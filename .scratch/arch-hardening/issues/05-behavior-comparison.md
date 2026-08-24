# T-05 behavior comparison: InlineMaterializer vs ferry-materialize::Applier

Read side by side before flipping the v1 engine over (`crates/ferry-sync/src/materialize.rs`
vs `crates/ferry-materialize/src/apply.rs`). Every difference below was either ported
into Applier BEFORE the flip or judged a safe superset.

| Behavior | InlineMaterializer | Applier | Action |
|---|---|---|---|
| File write path | temp in dest dir, mode+mtime on temp, sync_all, atomic rename | same, PLUS blake3 verify of every chunk after store read AND re-read of the temp pre-rename | none (superset) |
| Empty files | zero chunks materialize as empty files, no store fetch | same | none |
| Upsert ordering | buckets concatenated, `create_dir_all` parent per entry (implicit parents) | one ascending sort, parents-first; never touches unlisted parents | none — `diff_manifests` flattens added subtrees per-path incl. ancestor dirs (diff.rs `flatten_added_entry`), so the contract holds; suites prove it |
| Removal ordering | ascending sort reversed = children-first; `remove_dir` w/ `remove_dir_all` fallback | descending sort children-first; deep teardown recursive children-first | none |
| Type changes | old incarnation retired in removal phase, then after-state upserted | identical shape | none |
| Dir mtimes | **phase 3 stamps EVERY directory from the TARGET tree, deepest-first** — required for convergence because `diff_nodes` deliberately omits dir-mtime-only changes | only dirs that appear in the change set get touch-planned; ancestors of modified files keep wall-clock mtimes → root ids would diverge forever | **PORTED**: `Applier::restore_dir_mtimes_from_tree` + combined `apply_session_change_set` |
| Identical symlink (same target) | keeps working link, refreshes ITS OWN mtime | plans `Skip` outright — link-mtime-only drift (`metadata_modified`) would oscillate forever, never converge | **PORTED**: planner now emits `RestoreSymlinkMtime` (unix) when target matches but recorded times drifted |
| Symlink creation | `symlink()` in place | temp + rename + dir fsync | none |
| Symlink-target policy | **NONE** — `/etc`, `../../outside`, `C:x` passed straight to `symlink()` (T-05 audit finding, High) | `ferry_platform::classify_link` + `reject_windows_dir_link` at execute time | engine path INHERITS policy by routing through Applier; regression tests added |
| Symlink own-mtime restore | `set_symlink_times` (shared since T-012) | same | none |
| NFC live-folding hooks | none — bare component join misses NFD-on-disk spellings on byte-preserving hosts | `abs_under`/`live_nfc_match` resolve stored NFC names to live spelling | none (superset; this is the "NFC fixes landed in only one" drift) |
| Component validation | none | traversal defense (`..`, `/`, `\`, `\0`, `:`), Windows reserved device names, case-fold collision gate | none (superset security) |
| Exec bit | unix 0o755/0o644 only, other bits dropped by design; non-unix no-op | same convention; exec drift does not force rewrite on non-unix | none |
| Pre-epoch mtimes | manual i64/u32 → SystemTime math | equivalent `split_unix_time`/`system_time` pair | none |
| Blob access | `&dyn BlobSource` trait (fake-friendly tests) | `&Store` directly | BlobSource indirection dies with materialize.rs; both call sites hold `&Store`; adapter takes `&Store` |
| Temp naming | `.ferry-m0-tmp-{pid}-{seq}-{hash}` | `TempStyle`-based names, filtered by `is_temp_name` during live walks | none |
| Overwrite guard | unconditional | default `Overwrite::Always` = unconditional | none |
| Quarantine-name exemption | absent | absent (live walks ignore only applier temp names) | N/A — nothing to port |
| Pin `hold_filter` callback surface | absent | absent (that seam lives in ferry-pin/ferry-cli, not in either applier) | N/A — nothing to port |

## Ports landed in ferry-materialize BEFORE the flip

1. `Applier::restore_dir_mtimes_from_tree(root_tree_id)` — walk the target tree,
   stamp every directory's mtime deepest-first (skips no-op sets).
2. `Applier::apply_session_change_set(cs, target_root_tree_id)` — change-set apply
   followed by (1): exactly the contract the v1 pull stages need.
3. `Mutation::RestoreSymlinkMtime` — identical-target links whose own mtime drifted
   get their times restored instead of being skipped (planned/executed on unix only;
   non-unix cannot set link times and must not mutate forever).

## Regression coverage for the symlink-policy gap

`crates/ferry-sync/tests/symlink_policy.rs` drives `SessionApplier` — the exact
adapter both v1 pull paths call — with hostile targets (absolute `/etc/passwd`,
`..`-escaping, windows drive-prefixed `C:x`) and requires loud
`MaterializeError::SymlinkRefused` plus zero filesystem effect, plus a benign
positive control. These survive the deletion of materialize.rs.
