# T-009: iroh transport, LAN discovery, blind relay

Status: ready-for-agent
Depends on: T-008

Swap the localhost transport for iroh QUIC connections addressed by device
public key (ADR-0003). Isolate behind a `Transport` trait. Add multicast LAN
discovery and a self-hostable relay binary (dumb ciphertext pipe) used as
fallback when hole punching fails; clients retry direct periodically.

Acceptance: two machines behind separate home NATs (test via cloud VMs +
phone hotspot if needed) sync through relay then upgrade or stay direct per
iroh's negotiation; relay logs contain no plaintext.
