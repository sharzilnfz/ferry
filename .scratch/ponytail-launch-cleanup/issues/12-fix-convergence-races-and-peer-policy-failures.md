# Ticket 12: Fix convergence race conditions and peer-policy sync test failures

Status: ready-for-agent
Depends on: 11
Blocks: noise-free `cargo test --workspace`

## What to build

Address root causes causing sync loopback tests (`exchange_loopback.rs`) and UI server synchronization tests (`ui_server_tests.rs`) to fail or hang during convergence:

1. **Stdio pipe closure & mutex poisoning in `ferry-cli` loopback tests**:
   - `crates/ferry-cli/tests/exchange_loopback.rs` reads `LISTENING` from daemon stdout and drops the pipe handle.
   - Any background daemon logging to stdout via `println!` triggers `failed printing to stdout: Broken pipe (os error 32)`.
   - In `crates/ferry-sync/src/engine.rs`, `Ctx::status` logged to `println!`, causing session threads to panic while holding `session_lock`, poisoning the mutex and stalling all future sessions.
   - Fix: Route daemon status logging to `eprintln!` and ensure `session_lock` acquisition handles or prevents panics.

2. **Asymmetric adoption of obsolete peer manifests on local wins / deletions**:
   - In `crates/ferry-sync/src/exchange.rs` (`my_pull_stage`), when `outcome.held == 0 && !outcome.diverged`, the node unconditionally calls `self.adopt(target, man_bytes, man)`.
   - When a node deletes a file locally, `reconcile` chooses `Decision::KeepLocal`. Because deletions have no chunks to send, `plan.send` is empty, causing `diverged` to evaluate to `false`.
   - The node that performed the deletion was adopting the peer's older manifest containing the deleted file, resurrecting it and preventing convergence.
   - Fix: Expose `has_local_wins` from `ActionPlan` / `ConvergenceResult` (set when `KeepLocal` or `Conflict { winner: Side::Local }` occurs). Prevent agreement recording on the remote manifest when `has_local_wins` is true, and mark `diverged = true` so the deleting node preserves its local manifest.

3. **Event-driven scanner latency on automated test mutations**:
   - In `crates/ferry-scan/src/engine.rs`, `scan_once()` only drains pending events from the watcher queue.
   - If OS file notification events (such as macOS FSEvents) are debounced or not delivered immediately before an ad-hoc session starts, `scan_once()` returns `published: None` without checking disk, causing the daemon to offer a stale manifest.
   - Fix: When watcher signal queue is empty in `scan_once()`, execute a lightweight `stat_sweep` against the cache to detect dirty, modified, or deleted paths without re-hashing unmodified files.

4. **Peer policy override by store config in `ui_server_tests.rs`**:
   - `crates/ferry-cli/tests/ui_server_tests.rs` (`test_api_status_peer_agreement_when_nodes_synchronize_and_diverge`) sets `engine.set_peer_policy(PeerPolicy::TrustOnFirstUse)`.
   - In `crates/ferry-sync/src/engine.rs`, `current_policy()` refreshes policy from disk via `resolve_peer_policy_from_disk`. Because `open_or_create_test_store` creates a config head with only the local device identity, `resolve_peer_policy_from_disk` returns `AllowList({ self_id })`.
   - Because `refreshed != PeerPolicy::default()`, `current_policy()` discarded the programmatic `TrustOnFirstUse` policy and fell back to `AllowList({ self_id })`. With no remote peers in the allow-list, `expected_peer` refused all incoming/outgoing connections with `allow-list names no paired peer`.
   - Fix: Ensure `current_policy()` respects explicit `peer_policy` set on the engine (or when set to `TrustOnFirstUse`).

5. **Bogus peer dial addresses**:
   - In `crates/ferry-sync/src/transport.rs`, default `Transport::dial_peer` converted public key bytes to arbitrary IPv4 addresses, causing 5-second connection hangs per discovered peer.
   - Fix: Return `io::ErrorKind::Unsupported` unless an explicit routing table exists.

## Acceptance

- [ ] `cargo test -p ferry-cli --test exchange_loopback` passes consistently in < 10s.
- [ ] `cargo test -p ferry-cli --test ui_server_tests` passes cleanly.
- [ ] `cargo test -p ferry-scan` passes with 0 regressions.
- [ ] `cargo test -p ferry-sync-engine` passes with 0 regressions.
- [ ] `cargo test --workspace` passes cleanly on an idle machine.

## Comments

Root causes and minimal reproduction paths were isolated during investigation of live testing fixes. Detailed log captures and call graphs are recorded in the conversation transcript.
