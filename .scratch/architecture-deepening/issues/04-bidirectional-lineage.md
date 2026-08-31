# 04: Bidirectional lineage with empty-base degradation

**What to build:** Three-way reconciliation proves the agreed base is an
ancestor of both the local and remote manifest trees. One ancestor-walk helper,
called once per side, walks parent pointers from each side to the base. If
either walk fails to reach the base, the base degrades to empty: every file on
both sides is treated as a preserved addition and nothing is pruned. The
wall-clock timestamp fallback is deleted; a broken lineage never resolves to
diffing against the remote manifest. The helper's return type tells the truth
about what it can return, and the local manifest becomes a load-bearing input.

**Blocked by:** None (can start immediately).

**Status:** done

- [x] Base must be proven reachable from both local and remote manifest trees
- [x] Broken lineage on either side degrades to an empty base; all files on both sides survive as additions
- [x] The timestamp fallback is deleted; no broken-lineage path diffs against remote
- [x] One ancestor-walk helper serves both sides; no duplicated parent walk
- [x] The base-resolution interface's return type matches its real output space
- [x] Tests simulate local rollback (base ancestor of remote, not local) and assert local files are preserved in the plan
- [x] Existing anti-rollback tests pass
