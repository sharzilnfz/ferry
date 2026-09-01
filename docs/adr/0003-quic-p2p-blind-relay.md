# ADR-0003: QUIC peer-to-peer with a blind relay fallback

Status: accepted (2026-08-23)

## Context

Two developers' machines behind home NAT must connect over the internet
without port forwarding. Field-proven patterns exist in Syncthing (STUN,
discovery, relays), Tailscale (DERP: relay first, upgrade to direct, peers
addressed by public key), and iroh (Rust, QUIC hole punching, ~90% direct
success). Building NAT traversal from scratch is a multi-month detour.

## Decision

- Peers are addressed by device public key, never by IP.
- Connections start through a relay and opportunistically upgrade to direct
  QUIC paths via hole punching; the relay stays as fallback.
- Use the iroh library for traversal + transport rather than hand-rolling.
- Ship a self-hostable relay server from v0. A hosted community relay may run
  later but is never required for function, only for convenience.
- LAN discovery (multicast) shortcuts everything when devices share a network.

## Amendment (2026-09-01): Default mDNS & Supervisor Autonomous Dialing

Local network mDNS service advertisement/discovery is enabled by default on
all sync transport endpoints. The background daemon supervisor inspects
`CONFIG_HEAD` authorized peer keys and cross-references discovered routing
tables, automatically initiating bidirectional sync sessions without
requiring manual `--peer-url` or `--listen` CLI flags.

## Consequences

- Dependency on iroh's API stability; pin versions, isolate behind an internal
  transport trait so it can be swapped.
- Relay bandwidth is someone's cost. The Tailscale model (OSS core, optional
  paid infrastructure) is the eventual business shape, out of scope for v0.
