# 01: Low-Hanging Ponytail Reductions & Shared Platform Helpers

**What to build:**
Delete dead prototype code, consolidate duplicated civil UTC formatting and parsing into `ferry-platform::time`, unify hex formatting into `ferry-store::format::hex`, and remove single-caller wrappers across crates. This delivers a clean, shared foundation without behavioral regressions.

**Blocked by:** None (can start immediately)

**Status:** closed

- [x] Delete dead M0 proto sketch `crates/ferry-sync/src/proto.rs` (472 lines) and M0 tag state `crates/ferry-sync/src/state.rs` (28 lines).
- [x] Hoist `civil_utc`, `civil_from_days`, `fmt_rfc3339`, `now_unix`, and `parse_rfc3339_to_unix` into `crates/ferry-platform/src/time.rs`.
- [x] Remove duplicate `timefmt.rs` files in `ferry-daemon/src/timefmt.rs`, `ferry-daemon/src/ui/timefmt.rs`, `ferry-tui/src/timefmt.rs`, and `ferry-sync-engine/src/timefmt.rs`, redirecting callers to `ferry-platform`.
- [x] Replace hand-rolled hex formatting functions in `ferry-crypto`, `ferry-relay`, `ferry-daemon`, and `ferry-iroh` with `ferry_store::format::hex`.
- [x] Remove dead test helpers in production code (such as `RecoveryExport::round_trip_through_files` in `ferry-crypto`) and unused methods (`socket_path_for_folder_id`, `set_auto_remove` in `ferry-ipc`).
- [x] Full workspace build and test suite passes (`cargo test --workspace`).
