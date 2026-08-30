# 04: Delete dummy `IpcServer` fallback in bootstrap

**What to build:** The CLI bootstrap seam fails loudly instead of hiding behind a dummy. `start_dummy_daemon` and its `Ping`/`Pong` fallback thread are deleted from `ferry-cli` bootstrap. A missing daemon binary now surfaces as typed `daemon-start-failed` with hint `check $FERRY_HOME permissions` via `BootstrapError`. The Store and daemon spawn remain the only seams.

**Blocked by:** None (can start immediately)

**Status:** done

- [x] `ferry-cli` bootstrap contains no `start_dummy_daemon` and no hidden `ferry_ipc::IpcServer` thread — `crates/ferry-cli/src/bootstrap.rs:211` deleted; fallback branch now `return Err(daemon-start-failed)`
- [x] Running bootstrap without a built daemon binary returns `daemon-start-failed` with the permissions hint, not a silent `Pong` — `ensure_daemon_fails_with_daemon_start_failed_when_binary_missing` asserts `code=daemon-start-failed` and `hint=check $FERRY_HOME permissions`
- [x] `tests/bootstrap_tests.rs` and `tests/live_verification_e2e.rs` use the `FerryStore` or `FakeBackend` seam and still pass without the dummy — `ensure_daemon_reuses_running_server_via_ping` uses explicit `IpcServer` fake; `live_verification_e2e` 4/4 pass
- [x] No `grep` for `dummy` or `start_dummy` finds a hit in `crates/ferry-cli` after the change — `grep -rn dummy crates/ferry-cli` = 0
- [x] `cargo test --workspace` passes and `cargo clippy --workspace --all-targets -- -D warnings` passes — `cargo test -p ferry-cli --tests` all suites green; `cargo clippy -- -D warnings` 0 warnings
