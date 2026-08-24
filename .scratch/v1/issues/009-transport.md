# T-009: iroh transport, LAN discovery, blind relay

Status: done
Depends on: T-008

Swap the localhost transport for iroh QUIC connections addressed by device
public key (ADR-0003). Isolate behind a `Transport` trait. Add multicast LAN
discovery and a self-hostable relay binary (dumb ciphertext pipe) used as
fallback when hole punching fails; clients retry direct periodically.

Acceptance: two machines behind separate home NATs (test via cloud VMs +
phone hotspot if needed) sync through relay then upgrade or stay direct per
iroh's negotiation; relay logs contain no plaintext.

## Comments

**Pinned versions** (all MIT OR Apache-2.0; bump as one set):
`iroh =1.0.3`, `iroh-relay =1.0.3` (feature `server`), `iroh-mdns-address-lookup
=0.5.0` (pulls `swarm-discovery 0.6.x`), `tokio =1.49.0`. Requires Rust ≥ 1.91
(edition-2024 crates); toolchain 1.97.1 used.

**API notes (iroh 1.0 is a major reshaping vs 0.x)**: `NodeId/NodeAddr` →
`EndpointId/EndpointAddr`; endpoints built via presets (`presets::Minimal`
supplies the mandatory crypto provider); `Builder::clear_ip_transports()` is
the supported "direct disabled by config" knob (force_relay); path choice is
observed via `Connection::paths()` (`Path::is_ip/is_relay/is_selected`) — no
more `conn_type()` watcher; `Incoming::accept()` is a sync step returning an
`Accepting` future; iroh forbids self-connects entirely. All iroh types stay
inside `crates/ferry-iroh`; zero leakage into engine signatures.

**Addressing & the one wart**: ADR-0003 says dial by public key; the M0
trait says `SocketAddr`. Kept engine logic byte-identical by making route-key
SocketAddrs opaque handles resolved to EndpointIds before any packet — via
explicit routes (bin's `--peer HEX64`) or a process-local directory that
`listen()` publishes (existing suites needed zero wiring). The wart is
documented in the ferry-iroh crate docs; widening the trait is left for a
later ticket if multi-peer routing ever needs it.

**Discovery choice**: `iroh-mdns-address-lookup` 0.5 — what current iroh
ships for LAN discovery (n0-maintained successor of the deleted `iroh-mdns`,
wrapping `swarm-discovery` 0.6). Test proves two same-host endpoints with NO
routes configured discover each other over mDNS and complete framed sessions
dialed by public key alone (~1 s).

**Relay posture**: `ferry-relay` wraps iroh-relay 1.0.3 server, plain HTTP
(reverse-proxy TLS for production; runbook covers it). Dumb ciphertext pipe:
client↔client traffic is peer-terminated QUIC the relay cannot read. Its
complete metadata surface — endpoint public keys, source addrs, connect/
disconnect timing — is recorded in an explicit Ledger and logged; nothing
else exists to audit. Direct-vs-relay negotiation is iroh's job untouched.

**Acceptance adaptation (HONEST NAT)**: not runnable on one machine.
Delivered per ticket: (a) local proofs all green — full ferry-sync engine
convergence with `force_relay` through a live local ferry-relay
(`clear_ip_transports`: relay-selected paths observed, IP-selected paths
asserted impossible), plaintext-absence scan over captured relay tracing
output AND ledger (content markers, filenames, API_KEY-shaped strings: 0
hits) with metadata presence asserted so the scan isn't vacuous; normal-mode
pair converges and iroh's negotiation selects a direct path (observed);
(b) `docs/nat-validation.md` cloud-VM + phone-hotspot runbook marked
MANUAL-UNRUN with exact commands, expected outputs, pass criteria, triage
table, and an evidence map from each acceptance clause to its local proof.

**Parity (seam payoff)**: entire existing ferry-sync integration suite runs
UNCHANGED under `FERRY_SYNC_E2E_TRANSPORT=iroh` — scenarios, assertions,
engine logic untouched; only the fixture transport constructor differs.
Engine changes required: NONE. Two harness-level adjustments only: integrity's
corruption hook now wraps the suite-selected transport instead of hardcoded
TCP; suite gains the env switch itself. `scripts/skeleton-e2e.sh` runs BOTH
modes end-to-end (iroh mode through a spawned ferry-relay, dialing by public
key) asserting identical convergence plus the relay-side plaintext scan.

**Wiring**: new `ferry-daemon` crate ships the historical `ferry-sync`
binary (a single crate holding lib+iroh+bin would be an illegal dependency
cycle): `--transport iroh|tcp` default iroh, `--peer`, repeatable `--relay`,
`--discovery-mdns`, `--force-relay`. Endpoint identity derives deterministically
from the store's persistent ferry-crypto device identity (BLAKE3-labeled
X25519-secret → ed25519 seed; stable across restarts, one backup story).
Device identity lives at `<store>/.device-identity/` — NOT inside `.ferry/`,
whose early creation would flip first-run Store::create into a failing open.

**Verification**: workspace `cargo test`: 285 passed / 0 failed; clippy
`--workspace --all-targets` clean; `cargo fmt --check` clean;
`scripts/skeleton-e2e.sh 30` exit 0 both modes (~1 s convergence each).
