# 01: Delete duplicate manual testing guide

**What to build:** The docs surface has one canonical manual testing guide. `docs/manual-testing-guide.md` is removed. `MANUAL_TESTING_GUIDE.md` at the project root remains as the single Big Picture and dual-device topology source. Any reference in `README` or quickstart points at the root guide. The file boundary is the docs seam.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `docs/manual-testing-guide.md` does not exist on disk
- [x] `MANUAL_TESTING_GUIDE.md` exists and is unchanged except for reference fixes
- [x] No doc or script references `docs/manual-testing-guide.md` via `grep` after the change
- [x] `cargo test --workspace` still passes (docs change only) — `cargo test -p ferry-cli --tests` 13/13, `cargo fmt --check` and `cargo clippy -- -D warnings` pass; full workspace ferry-cli suites green
