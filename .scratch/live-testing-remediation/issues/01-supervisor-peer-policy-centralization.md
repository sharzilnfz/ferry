# Ticket 01: Centralize Supervisor Peer Authorization via PeerPolicy

Status: done
Depends on:
Blocks: 08, 09, 10

## What to build

Address the ADR-0007 violation in `crates/ferry-daemon/src/supervisor/mod.rs:sync_discovered_routes`.

Currently, the supervisor manually constructs filesystem paths (`rec.path.join("config")` and `rec.path.join(".ferry").join("config")`), reads raw bytes, and filters `entry.device_pub != self.identity.public()`.

ADR-0007 mandates that peer derivation and allow-list resolution live strictly once on `PeerPolicy` beside its `CONFIG_HEAD` parser:

1. In `crates/ferry-daemon/src/supervisor/mod.rs`, update `sync_discovered_routes` to parse `PeerPolicy::from_config_head(&bytes)` or query the folder engine's active `peer_policy()`.
2. Extract remote authorized peers via `policy.remote_peers(self.identity.public())`.
3. For each authorized peer, register it in the transport route table if not already resolved.
4. Remove manual configuration path loops and manual public key inequality checks.

## Acceptance

- [x] `supervisor.sync_discovered_routes()` derives peer identities via `PeerPolicy` methods only.
- [x] No manual filesystem parsing or self-identity checks duplicate `PeerPolicy` logic.
- [x] `cargo test -p ferry-daemon --test supervisor_tests` passes cleanly.

## Comments

Identified as the worst Standards finding in the `live-testing-fixes` code review.
