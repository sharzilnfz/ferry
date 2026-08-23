# T-014: Engine runs exchange protocol v1 with encryption on

Status: ready-for-agent
Depends on: T-006, T-007, T-008
Blocks: T-013

Close the loop between the M0 walking skeleton (`crates/ferry-sync`) and the
encrypted wire protocol (`crates/ferry-proto`, spec at
`docs/store-format.md` §Wire protocol v1). The engine keeps its `Transport`
seam but replaces the throwaway plaintext message set with ferry-proto
sessions: device-key mutual auth, per-direction AEAD traffic keys, manifest
adverts, verified pack/item transfer, agreement records. The pass-through
M0 framing stays available only behind an explicit dev flag for debugging.

Acceptance (from T-008's ticket, made concrete here):
- `scripts/skeleton-e2e.sh` passes with encryption ON by default.
- A flipped in-flight byte fails authentication, is never written, and the
  next poll round converges.
- Agreement ledgers land on both sides in the canonical record format.

Notes: T-009 owns the transport underneath (iroh); do not overlap — branch
after T-009 merges. T-010 owns reconciliation semantics; keep M0's
empty-vs-nonempty bootstrap rule until T-013 wires the real engine.
