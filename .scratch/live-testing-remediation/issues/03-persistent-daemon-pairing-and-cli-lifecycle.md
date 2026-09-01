# Ticket 03: Persistent Daemon Pairing Hosting and Interactive CLI Share Lifecycle

Status: completed
Depends on:
Blocks: 04, 10

## What to build

Fix the premature exit bug in `ferry share` where ephemeral CLI processes kill the pairing listener before remote joiners can complete the handshake:

1. **Persistent Daemon Pairing Service**:
   - In `crates/ferry-daemon/src/ipc/mod.rs` and `state.rs`, ensure `CreatePairingSession` spawns a background task managed by the daemon that listens on the pairing rendezvous channel for the full session TTL (or until consumed).
   - When a remote joiner contacts the daemon over the rendezvous channel, the daemon completes the handshake, records the peer's wrapped key in `CONFIG_HEAD`, and registers the peer in the route table.

2. **Interactive CLI `ferry share` Lifecycle**:
   - In `crates/ferry-cli/src/commands/share.rs`, when running in terminal mode without `--no-wait`, display the pairing short code and QR code and wait interactively for either:
     - The daemon to signal that a joiner connected and completed the handshake (via IPC polling or event stream).
     - The user to press `Ctrl+C` or `q` to exit (leaving the daemon pairing session active in the background).
     - The 24h pairing TTL to expire.

## Acceptance

- [x] `ferry share` delegates pairing session hosting to the background daemon so offers survive CLI termination.
- [x] Running `ferry share` interactively shows pairing code/QR and reports success as soon as `ferry join` completes.
- [x] Running `ferry join <CODE>` against a shared folder updates `CONFIG_HEAD` on both machines and initiates sync without errors.
- [x] Multi-process integration tests verify that `ferry share` on Machine A followed by `ferry join` on Machine B succeeds even with timing gaps between commands.

## Comments

Resolves Spec finding #2 (ephemeral CLI share listener termination).
