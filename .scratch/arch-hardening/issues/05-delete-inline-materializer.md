# T-05: Delete InlineMaterializer — engine applies through ferry-materialize::Applier

Status: ready-for-agent
Depends on: T-02 (chunker API settles first)

`crates/ferry-sync/src/materialize.rs` (~700 lines) is a second guarded
applier ("ugly-but-correct" per its own lib.rs comment) that is what actually
runs: `engine.rs:985` and `exchange.rs:488` construct `InlineMaterializer`,
while the battle-tested `ferry-materialize::Applier` only gets proved by
kill_safety tests. Duplication has already caused drift: commit 9c440a3 had
to patch BOTH appliers for windows dir-mtime restore, while exec-bit (3fe146f)
and NFC fixes landed in only one.

The `Materializer` seam exists precisely for this: point the v1 engine's
materializer construction at an adapter over `ferry-materialize::Applier`
(keep the existing `BlobSource` bridging store→applier), port anything
InlineMaterializer does that Applier doesn't (compare behaviors carefully —
quarantine-name exemption, NFC live-folding hooks, pin hold_filter callback
surface if present), then delete `ferry-sync/src/materialize.rs` entirely.

Preserve all public behavior of run_session_v1/tick. If Applier lacks a hook
InlineMaterializer relies on, extend ferry-materialize with it rather than
keeping two implementations.

ADDITIONAL SCOPE (post-ticket audit finding, High): InlineMaterializer's
`upsert_symlink` (materialize.rs ~303-324) creates symlinks from untrusted
manifest targets with NO policy — `/etc` or `../../outside` pass straight
through to `symlink()`. The Applier already enforces
`ferry_platform::classify_link` (apply.rs ~602-617) plus
reject_windows_dir_link, so routing the v1 path through Applier closes this;
add regression tests proving hostile targets (absolute, `..`-escaping,
windows drive-prefixed) are REFUSED loudly through the engine path, and keep
those tests passing after the deletion.

Acceptance: `rg InlineMaterializer` returns nothing; ferry-sync has no
materialize.rs; full convergence + protocol_v1 + kill_safety suites green;
windows-specific mtime/exec behavior comes from the single Applier; hostile
symlink-target regression tests green.
