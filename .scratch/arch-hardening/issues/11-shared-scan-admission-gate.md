# T-11: One per-entry admission gate shared by snapshot and walk

Status: ready-for-agent
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
