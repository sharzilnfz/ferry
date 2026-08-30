










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
