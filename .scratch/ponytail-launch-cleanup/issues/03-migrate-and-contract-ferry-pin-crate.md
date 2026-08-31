# 03: Migrate callers and delete ferry-pin crate

Status: ready-for-agent
Depends on: 02
Blocks: 08

**What to build:** A single source of truth for session pinning as a convergence policy, not a separate crate domain. From the user perspective `cargo install` and docs list one fewer crate and one fewer concept to learn. From the maintainer perspective a pin policy edit touches one module.

**Blocked by:** 02

**Status:** ready-for-agent

- [ ] All workspace dependents that previously depended on the facade crate now depend on the convergence engine crate and import session pinning from its `pin` module (migrated in one batch, crate graph drops by one)
- [ ] The facade crate is removed from the workspace manifest and from disk; text search for the old crate name in shipped code returns zero
- [ ] While a folder is pinned on one device, competing remote edits for held paths are withheld and persisted in the held ledger through daemon restart; releasing the pin converges and clears the ledger transactionally, verified through the convergence engine seam
- [ ] `cargo test` for convergence, folder lifecycle, and daemon supervisor seams passes; external behavior unchanged

## Comments

Wide-refactor contract step. This is the user-visible crate-count reduction that the audit flags as the biggest cut (about 828 lines). Follows `model-the-domain`: pinning is convergence policy.
