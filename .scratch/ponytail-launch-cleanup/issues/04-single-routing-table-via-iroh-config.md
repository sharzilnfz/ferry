# 04: Single routing table via IrohConfig

Status: ready-for-agent
Depends on: 01
Blocks: 05

**What to build:** A single routing registry for the transport seam so an operator adding a route for a folder sees one truth and a developer debugging a dial checks one place. From the user perspective relay fallback and direct QUIC agree. From the maintainer perspective dial resolution has one map to audit.

**Blocked by:** 01

**Status:** ready-for-agent

- [ ] The process-global routing table is deleted; the instance routing table injected strictly via `IrohConfig` routes is the only registry; dial resolves only through the instance table with no global fallback per `separate-before-serializing-shared-state`
- [ ] Adding a route for a folder via `IrohConfig` and dialing succeeds end-to-end (direct when possible, relay fallback otherwise) verified through the transport seam
- [ ] No second routing map stores the same route key; text search for the old global symbol in shipped code is zero
- [ ] No wire or store format change; ADR-0003 QUIC + relay transport stays behind the transport trait

## Comments

Tracer-bullet vertical slice through the transport boundary. Already exercises the highest seam so no new seam is added. Pairing rendezvous (05) blocks on this because it shares `IrohConfig` routing.
