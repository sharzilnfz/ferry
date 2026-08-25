//! Lockstep guard (T-14 follow-up hardening): the store-level snapshot
//! defaults duplicated in `ferry_store::snapshot::DEFAULT_IGNORE` must never
//! drift from the canonical product decision in
//! `ferry_ignore::defaults::DEFAULT_RULES` (CONTEXT.md, "Selective rules").
//!
//! `snapshot_dir` is `SyncEngine`'s default snapshot source and cannot see
//! per-folder rule sets, so it carries its own copy of the baseline list.
//! This test is the tripwire: change one list, you must change both — and
//! changing the list at all is a product decision, not a code cleanup.

/// The canonical default exclude lines (`ferry-ignore` crate docs).
use ferry_ignore::defaults::DEFAULT_RULES as CANONICAL_DEFAULT_RULES;

#[test]
fn store_snapshot_defaults_match_canonical_default_rules() {
    assert_eq!(
        ferry_store::snapshot::DEFAULT_IGNORE,
        CANONICAL_DEFAULT_RULES,
        "ferry-store's snapshot DEFAULT_IGNORE drifted from \
         ferry-ignore's DEFAULT_RULES; keep both in lockstep or unify them"
    );
}
