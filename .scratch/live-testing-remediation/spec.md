# Live Testing & Standards Remediation Spec

Status: ready-for-agent

## Problem Statement

Following the initial implementation of zero-friction network pairing, discovery, and daemon autostart, a rigorous two-axis code review (Standards and Spec) surfaced critical gaps and regressions:

1. **Supervisor Policy Bypass**: The daemon supervisor manually reads filesystem configuration and filters peer identities, bypassing the centralized `PeerPolicy` interface mandated by ADR-0007.
2. **Ephemeral CLI Share Lifetime**: Running `ferry share` prints a pairing code and exits immediately. Because the pairing offer listener runs only in the transient CLI process, remote `ferry join` commands fail when attempting to complete key exchange across devices.
3. **Pairing Secret Exposure**: Secret pairing codes and offer payloads in Web UI HTTP handlers are converted to raw heap strings without cryptographic memory zeroization, violating ADR-0002 and ADR-0006.
4. **Web UI PID False Positives**: The CLI web token discovery query performs PID liveness checks against incorrect path directories, risking false positives against recycled OS process IDs.
5. **Relay-Resilient Network Pairing**: Pairing offers currently use local subnet UDP multicast rather than Iroh peer-to-peer rendezvous topics, preventing devices on separate subnets, Tailscale meshes, or NAT environments from pairing without prior local network proximity.
6. **Code Duplication & Architectural Smells**: Core rendezvous socket logic is duplicated between folder and transport modules; IPC command dispatchers contain redundant match branches; and store agreement ledgers perform string-based path introspection.

## Solution

Ferry provides a hardened, standards-compliant, and relay-capable pairing and sync architecture:

1. **Centralized Peer Authorization**: The daemon supervisor consumes canonical `PeerPolicy` methods to derive authorized remote peers for routing table synchronization.
2. **Persistent Daemon Pairing Service & Interactive CLI**: `ferry share` delegates pairing offer hosting to the background daemon or maintains an interactive waiting loop, allowing remote joiners to complete mutual key exchange reliably.
3. **Zeroized Memory for Web UI Secrets**: All Web dashboard endpoints holding short codes, pairing tokens, or keys wrap sensitive payloads in zeroizing memory buffers.
4. **Robust Web Session Verification**: Web UI token lookup validates session metadata directly against the root project directory's daemon PID lock.
5. **Iroh Topic-Derived P2P Rendezvous**: 6-character pairing codes derive encrypted rendezvous topics over Iroh QUIC/relay streams, enabling pairing across arbitrary networks and NAT topologies.
6. **Deduplicated Modules & Clean Store Interfaces**: Rendezvous transport logic is consolidated into the transport crate; IPC dispatch handlers are unified; and store ledger interfaces receive explicit filesystem roots.

## User Stories

1. As a developer running `ferry share ~/my-project`, I want the command to keep the pairing session alive until a remote device joins or the session is cancelled, so that the pairing offer is not terminated prematurely.
2. As a developer pairing two machines across different networks or behind NATs, I want `ferry share` and `ferry join` to rendezvous over encrypted P2P topics, so that I do not need to be on the same physical WiFi subnet.
3. As a developer joining a shared folder with `ferry join <CODE> ~/my-project`, I want the pairing ritual to complete mutual key exchange and update `CONFIG_HEAD` allow-lists on both machines, so that subsequent daemon sync succeeds automatically.
4. As a security-conscious engineer, I want short pairing codes and decrypted folder keys in the Web UI process to be zeroized from memory after use, so that sensitive credentials do not linger in memory dumps.
5. As a maintainer auditing access control, I want the daemon supervisor to query `PeerPolicy` directly rather than walking filesystem configurations manually, so that peer authorization rules remain unified in a single location.
6. As a developer running `ferry ui token`, I want the command to verify whether the recorded Web server process is genuinely active before outputting credentials, so that stale session files from terminated processes are cleanly invalidated.
7. As a Web UI user clicking "Share Folder", I want the dashboard to display the 6-character code and QR code backed by a persistent daemon pairing session, so that joining from another browser or device succeeds immediately.
8. As a Web UI user clicking "Join Folder", I want to enter a 6-character code and destination path and receive real-time pairing progress via event streams, so that I know exactly when synchronization begins.
9. As a Web UI user viewing discovered network devices, I want clicking "Pair" to initiate the authenticated pairing handshake over the local route table, so that connection setup requires only one click.
10. As a developer inspecting daemon logs, I want IPC command dispatching to provide consistent error reporting and execution paths across single-folder and multi-folder supervisor modes, so that behavior is completely uniform.
11. As a store engine developer, I want the agreement ledger to operate on explicit store paths without inspecting path string patterns, so that storage abstractions remain independent of caller directory structures.
12. As a developer running `ferry pin start` and `ferry pin release`, I want incoming remote changes held during the pin session to be saved in the blob store and reconciled cleanly upon release, so that no changes are lost.

## Implementation Decisions

1. **Supervisor Policy Integration**: The supervisor queries `PeerPolicy::from_config_head` or the active folder engine's policy interface to derive remote peers for route registration, eliminating duplicate manual configuration parsing.
2. **Daemon Pairing Service Delegation**: When `ferry share` is invoked, the CLI sends a `CreatePairingSession` IPC command to the background daemon. The daemon hosts the encrypted rendezvous endpoint, monitors incoming join requests, and commits the resulting wrapped key to `CONFIG_HEAD`. The CLI process waits for completion with a user-friendly progress spinner and timeout.
3. **Encrypted Iroh Rendezvous Topics**: Pairing rendezvous uses Iroh's P2P gossip and relay channels keyed by the 6-character pairing code's cryptographic hash, supporting both local subnet broadcast and wide-area relay traversal.
4. **Zeroized Pairing Memory in Web UI**: Web dashboard route handlers use memory-zeroing types (`Zeroizing<String>`) for all pairing codes, token strings, and QR rendering buffers.
5. **Path-Correct Web Session Validation**: Web session file parsing verifies PID validity against `.ferry/daemon.pid` relative to the folder root rather than nested `.ferry` subpaths.
6. **Transport Module Consolidation**: All network rendezvous, framing, and socket binding implementations are consolidated in the transport crate; the folder crate consumes these primitives through public interfaces.
7. **Unified IPC Command Matching**: Shared IPC command branches between client and supervisor dispatchers are extracted into a common handler function.
8. **Explicit Store Ledger Directories**: `AgreementLedger` accepts an explicit store path parameter and operates purely on content-addressed store semantics without string-matching filesystem paths.
9. **Strict Scope Control**: Non-essential audio synthesis additions in frontend assets are removed or isolated behind optional UI polish configurations.
10. **Deferred Test Suite Isolation**: Test suite stabilization items (Ticket 11 and Ticket 12) remain tracked as separate test hardening tickets and do not block core standards remediation.

## Testing Decisions

- **Testing Philosophy**: Tests must exercise public CLI commands, IPC socket protocols, HTTP endpoints, and observable filesystem state on disk. Tests must not inspect private internal fields or bypass public APIs.
- **Seam 1: CLI and IPC Integration**: Multi-process tests verify `ferry share` and `ferry join` end-to-end over network sockets, asserting mutual `CONFIG_HEAD` allow-list updates and subsequent sync session authorization.
- **Seam 2: Web Dashboard API & SSE**: Integration tests verify `/api/share`, `/api/pair/join`, and `/api/status` endpoints, checking authentication enforcement, JSON schema compliance, and event stream broadcasts.
- **Seam 3: Supervisor & Policy Seam**: Supervisor unit and integration tests assert that discovered peers are registered into route tables strictly when authorized by `PeerPolicy`.
- **Prior Art**: Builds on integration test fixtures in `crates/ferry-cli/tests/` and `crates/ferry-daemon/tests/`.

## Out of Scope

- Fixing third-party NAT relay hosting or dedicated cloud relay deployments.
- Multi-user role-based access control (RBAC) beyond device allow-lists.
- Concurrent sync test stabilization covered under deferred Tickets 11 and 12.

## Further Notes

- All changes maintain strict compatibility with ADR-0001 through ADR-0008.
- Pairing codes strictly follow ADR-0006 Base32 format with CRC32 checksums.
