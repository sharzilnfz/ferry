# 06: Single cipher seam

Status: ready-for-agent
Depends on: 01
Blocks: 09

**What to build:** One wire cipher seam so a crypto fix ships once. From the security auditor perspective handshake and application data share the same KDF and nonce discipline at the `ferry-proto` boundary. From the user perspective encrypted blob exchange is unchanged.

**Blocked by:** 01

**Status:** ready-for-agent

- [ ] Duplicated direction cipher in the sync session is deleted; session establishment calls the proto cipher directly as the single implementation
- [ ] KDF and nonce construction agree with the proto seam; an encrypted manifest and blob round-trip through the session seam succeeds and is verified via interop tests
- [ ] Text search for the duplicated cipher symbol in shipped code is zero
- [ ] No wire format or store encryption change per ADR-0002 and ADR-0005

## Comments

Tracer-bullet through the session and proto boundary per `boundary-discipline` and `encode-lessons-in-structure`. No new trait; reuse the existing proto cipher seam. This is the smallest P1 vertical slice and can run parallel to routing and backend work.
