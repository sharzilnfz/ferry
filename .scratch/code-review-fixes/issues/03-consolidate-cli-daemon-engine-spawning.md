# 03: Consolidate CLI daemon ad-hoc engine spawning

**What to build:** Refactor `ferry-cli/src/commands/daemon.rs` when `--listen` or `--peer-url` is specified to avoid duplicating store opening, polynomial derivation, and engine configuration logic. Reuse `ferry-daemon::supervisor::Supervisor` or unified device daemon routines so engine construction and supervision logic exist once in the daemon crate.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] Duplicate store opening and polynomial validation in `ferry-cli/src/commands/daemon.rs` are deleted
- [ ] Ad-hoc CLI daemon runs delegate engine construction and supervision to the supervisor or unified daemon interface
- [ ] CLI daemon tests in `crates/ferry-cli/tests/` and unit tests in `daemon.rs` pass cleanly
