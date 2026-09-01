# Ticket 06: Deduplicate Network Rendezvous Wire Functions

Status: ready-for-agent
Depends on: 04
Blocks: 09, 10

## What to build

Consolidate duplicated wire framing and discovery socket implementations across the workspace:

1. **Rendezvous Transport Unification**:
   - `crates/ferry-folder/src/pairing.rs:1096-1320` and `crates/ferry-iroh/src/rendezvous.rs:80-285` contain near-identical copies of `bind_discovery_socket`, `start_pairing_server`, `client_discover_and_join`, `service_name_for_code`, `send_frame`, and `recv_frame`.
   - Consolidate all network framing, UDP multicast, and socket binding logic into `crates/ferry-iroh::rendezvous` (or a dedicated transport rendezvous module).
   - In `crates/ferry-folder/src/pairing.rs`, delete the duplicated wire functions and delegate discovery/transport operations through `ferry_iroh::rendezvous`.

2. **Clean Trait Boundary**:
   - Ensure `ferry-folder` depends on abstract rendezvous/transport interfaces rather than embedding raw socket primitives.

## Acceptance

- [ ] Network rendezvous socket and framing logic exists in exactly one module (`crates/ferry-iroh/src/rendezvous.rs`).
- [ ] `crates/ferry-folder/src/pairing.rs` contains zero duplicated socket loop or framing code.
- [ ] `cargo check --all-targets` and `cargo test -p ferry-folder -p ferry-iroh` pass cleanly.

## Comments

Resolves Standards finding #3 (duplicated rendezvous code).
