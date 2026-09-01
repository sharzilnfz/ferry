# Ticket 04: Encrypted Iroh Topic P2P Pairing Rendezvous

Status: ready-for-agent
Depends on: 03
Blocks: 06, 09, 10

## What to build

Extend network pairing from local-only UDP multicast to wide-area Iroh P2P rendezvous topics as specified in ADR-0003, ADR-0006, and `spec.md:L54`:

1. **Topic Derivation**:
   - Derive an Iroh gossip / rendezvous topic identifier deterministically from the canonical 6-character Base32 pairing code using BLAKE3: `topic = blake3::keyed_hash(PAIRING_TOPIC_KEY, code.as_bytes())`.

2. **P2P Rendezvous Channel**:
   - In `crates/ferry-iroh/src/rendezvous.rs`, connect the pairing offer publisher and consumer to the Iroh endpoint's gossip/relay channel in addition to local subnet UDP broadcast.
   - When `ferry share` publishes an offer, advertise the offer message on the Iroh topic and local multicast.
   - When `ferry join <CODE>` runs, subscribe to the Iroh topic and broadcast discovery probes, connecting over direct QUIC hole-punched connections or fallback Iroh relays.

3. **Fallback & Graceful Degradation**:
   - If Iroh network connectivity is unavailable (offline LAN mode), pairing cleanly falls back to local subnet UDP broadcast on port 44005.

## Acceptance

- [ ] Pairing handshake succeeds across separate network subnets and Tailscale meshes via Iroh rendezvous.
- [ ] Local subnet pairing continues to work offline without internet/relay access.
- [ ] End-to-end tests verify pairing over Iroh transport fixtures.

## Comments

Resolves the worst Spec finding (local-only multicast rendezvous falling short of relayed Iroh topic pairing).
