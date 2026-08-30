# 02: Expand ferry-pin shim alongside existing crate

Status: ready-for-agent
Depends on: 01
Blocks: 03, 08

**What to build:** A deep pin module that lives beside the existing facade so callers can migrate without breaking the workspace. From the user perspective a folder with session pinning held edits still land in the held ledger and survive restart. From the maintainer perspective there are temporarily two import paths for the same types but only one owns the implementation.

**Blocked by:** 01

**Status:** ready-for-agent

- [ ] Convergence engine crate exposes a `pin` module that owns session pinning types (pin manager, held ledger, held entry, pin record) and helpers (hold gating, matcher) with the same observable behavior as the facade crate
- [ ] Both the facade crate and the new module compile and expose identical public symbols behind a re-export shim so no downstream crate breaks before migration
- [ ] No downstream crate has been migrated yet in this ticket; behavior is verified through the convergence and session pinning seam (held edits gated during pin, released atomically) per `ferry-sync-engine` matrix and ledger tests
- [ ] No store, manifest, or pairing wire format changes

## Comments

Wide-refactor expand step per `migrate-callers-then-delete-legacy-apis` and `subtract-before-you-add`. Next ticket migrates callers batch-wise while both paths exist so CI stays green.
