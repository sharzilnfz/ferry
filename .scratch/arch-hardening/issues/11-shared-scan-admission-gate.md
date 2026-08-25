# T-11: One per-entry admission gate shared by snapshot and walk

Status: done
Depends on: T-09 (walk.rs rehash rewrite lands first)

ferry-store/snapshot.rs (NFC normalization, reserved device names, link
classification, case-collision detection, collision refusals) and ferry-scan/
walk.rs duplicate the same policy wiring — walk.rs's own header admits "walk
rules mirror snapshot.rs exactly". Equality currently rests on oracle TESTS,
not construction; recent fold-shadowed-rename fixes had to be reasoned about
twice.

Fix: extract the per-entry admission decision into ONE function (input:
parent-relative raw name + entry kind + lstat facts + sibling state; output:
normalized TreeEntry | Refuse(reason) | Collide(name) | Link classification),
owned by ferry-store next to the snapshot types (or ferry-platform if types
dictate). Both snapshot_dir and the incremental walker call it, so
"incremental == from-scratch" holds by construction. Preserve exact refusal
messages/error kinds (tests assert them).

Acceptance: adversarial fixture cases (fold-shadowed renames, reserved names,
non-UTF-8 names, case collisions, link escapes) run against the shared gate;
snapshot-vs-walk equality tests remain green; deleting either caller's
private copy of the rules compiles because none remains.

## Implementation note (T-11 landed)

Gate lives at `crates/ferry-store/src/admission.rs`, re-exported as
`ferry_store::admission` — owned by ferry-store next to the snapshot types
(`RefusalReason` stays there and passes through untouched), since every
input kind (`OsStr` names, lstat kinds, link targets) is already store-side
vocabulary; ferry-platform remains purely decisional. Two-phase API
(`admit_name` = UTF-8+NFC, `admit_kind` = reserved names + symlink policy +
representable kinds, composed one-shot via `admit`) because the incremental
walker must interpose its walker-local filters (store-dir exclusion, ignore
policy) between the phases: ignored entries are skipped silently, never
refused loudly. Sibling collision detection was already single-sourced
(`snapshot::ensure_no_collisions`) and is unchanged. Behavior notes:
refusal messages/kinds byte-identical; the incremental walker's non-UTF-8
ledger line now records the full parent-relative path (rel + lossy name),
matching snapshot semantics — previously it dropped the parent prefix.
Watcher-event NFC mapping in ferry-scan/engine.rs is event→RelPath
conversion with skip-not-refuse semantics, deliberately NOT routed through
the gate.
