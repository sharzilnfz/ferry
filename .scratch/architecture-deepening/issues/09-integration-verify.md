# 09: Integration verify on the feature branch

**What to build:** The branch is verified shippable as a whole. Every ticket's
acceptance criteria hold together on the feature branch, the quality gates
pass, and the full per-crate test suite is green. This ticket exists because
the slices land in sequence and the last one must prove the stack, not just
itself.

**Blocked by:** 02, 04, 05, 06, 07, 08.

**Status:** done

- [x] `cargo fmt --all --check` passes
- [x] `cargo clippy --workspace --all-targets` is clean
- [x] The full test suite passes per crate across the workspace, zero failures
- [x] Every acceptance criterion of the spec (`.scratch/architecture-deepening/spec.md`) is demonstrably met on this branch
- [x] The dual-device end-to-end flow from the manual testing guide still works: init, share, join, sync both ways
