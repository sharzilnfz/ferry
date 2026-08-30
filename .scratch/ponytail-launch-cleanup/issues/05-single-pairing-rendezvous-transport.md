# 05: Single pairing rendezvous transport

Status: ready-for-agent
Depends on: 04
Blocks: 09

**What to build:** One rendezvous path for the pairing ritual so a user typing a 6-char code (ADR-0006) never sees a filesystem fallback contradict a relay success. From the user perspective pairing either succeeds via mDNS topic and relay fallback or fails with one error. From the maintainer perspective `peek_session` checks one store.

**Blocked by:** 04

**Status:** ready-for-agent

- [ ] Dual rendezvous (in-memory shared map plus filesystem rendezvous file plus no-op advertise/discover stubs) collapsed to one transport: iroh mDNS topic and relay fallback per ADR-0003/ADR-0006; `PairingRitual` mint/answer succeeds end-to-end through that single path
- [ ] No second production path is checked alongside the primary; filesystem rendezvous file handling in shipped code is absent unless explicitly retained as a `#[cfg(test)]` in-memory test seam
- [ ] No-op stub symbols are absent in production builds
- [ ] Pairing code format (6-char base32, CRC-32 checksum, 24h expiry, constant-time verify) unchanged per ADR-0006; no store or manifest change

## Comments

Vertical slice through the folder lifecycle seam. If cross-process single-machine testing needs a seam, it is an explicit test constructor, not a second production registry. This ticket is the pairing half of the routing single-truth work.
