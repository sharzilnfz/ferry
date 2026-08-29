# 02: One device-daemon entry point

**What to build:** The device daemon has exactly one way to run. The daemon
crate exposes a single entry that takes the Ferry home, the device identity,
and the folder records, and owns signal handling, folder registration, the
supervision tick loop, and lock teardown. The CLI binary keeps argument parsing
and delegates; its copy-pasted signal watch block, registration loop, tick
loop, and lock error mapping are deleted. SIGINT and SIGTERM behave identically
however the daemon is launched. With the duplication gone, the formatting gate
passes again.

**Blocked by:** 01 (the entry point delegates lock teardown and stop/status to
the DaemonLock interface).

**Status:** ready-for-agent

- [ ] One function in the daemon crate runs the device daemon; the CLI delegates to it
- [ ] Signal handling, registration, tick loop, and lock teardown each exist once in the codebase
- [ ] SIGINT and SIGTERM produce identical cleanup whether launched directly or via CLI
- [ ] `cargo fmt --all --check` passes
- [ ] `cargo clippy` stays clean
- [ ] Existing CLI and daemon tests pass unchanged
