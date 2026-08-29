# 06: Watch converge.rs for divergent change

**What to build:** `crates/ferry-sync-engine/src/converge.rs` (~1650 lines) fuses decisions, pin gating, materialization, quarantine, and ledger — cohesive today, but flagged in review as the file most likely to be edited for unrelated reasons next.

**Blocked by:** None, but do not act without a concrete second reason to edit it.

**Status:** wontfix (until it actually diverges)

- [ ] If a future change edits converge.rs for a reason unrelated to convergence, split the pin-gating decision layer from the materialization/ledger pipeline at that point.
