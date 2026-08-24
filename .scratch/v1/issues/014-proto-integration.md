# T-014: Engine runs exchange protocol v1 with encryption on

Status: done
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

## Comments

**Found state (resumed worker).** The previous session died uncommitted,
leaving: `Cargo.toml`/`Cargo.lock` dep additions, `pub mod session;`, and an
untracked 942-line `session.rs` (Link/ConnLink/RawLink framing,
DirectionCipher, full handshake transcription, unknown-type policy, TOFU or
pin peer acceptance) with a unit-test module containing three latent bugs.
Committed as slice 1 after repairing the tests: duplex body-offset
assertions in the unsealed-mode test, a wrong-end pipe read plus an
unbounded `read_to_end` in the BYE(1) test (the duplex pipe only EOFs when
the peer half drops), and BYE(3) expectations that now distinguish the
pin-failing side from the side consuming its goodbye mid-handshake.

**What replaced what.**
- M0 HELLO/OFFER/REQ_META/REQ_DATA/AGREED/ITEM(S_DONE)/ERROR → v1
  HELLO/HELLO_ACK/AUTH_INIT/AUTH_CONFIRM + FOLDER_OFFER/INDEX_ADVERT +
  REQUEST_ITEMS/REQUEST_PACKS → ITEM_BATCH/PACK_ITEM + local agreement +
  BYE (`exchange.rs`, new).
- Donor/puller election → v1's role-serialized pull stages with BOTH plans
  computed from round-1 offers; adoption follows lineage
  (last-writer-wins) with M0's empty-vs-nonempty bootstrap guard intact
  plus a NEW stale-offer guard: adopting anything older than the current
  pointer is refused, which kills the regression ping-pong symmetric pulls
  would otherwise produce. `pick_donor`/`select_donor` stay exported for
  tests/T-010 reuse but no longer drive sessions.
- "Snapshot every tick" → adopt-and-hold: a scan whose root equals the
  current pointer mints nothing, so announced ids stay stable and
  comparable across peers (precondition for round-2 equality under v1).
- Materialize-then-AGREED → materialize BEFORE round 2; round 2 observes
  equality and plays AGREED's role. Pullers materialize durably first —
  same order as M0.
- Tag-derived fake device ids in manifests/handshake → real X25519
  identities via deterministic per-tag key derivation
  (`device_identity_for_tag`); stable across restarts; T-007 replaces
  provisioning, not protocol. `EngineConfig::expected_peer_id` adds strict
  pinning; default remains trust-on-first-use (possession proofs ALWAYS
  run either way).

**Legacy flag status.** `EngineConfig::legacy_m0_proto`, default OFF, wired
in `dispatch_session`; daemon passes it explicitly as false (no CLI surface
— programmatic only, exercised by `legacy_dev_flag_still_converges`).
`proto.rs` and the whole legacy session path are unchanged otherwise.

**Ledger compatibility.** Agreements write THE canonical 77-byte record via
`ferry_proto::agreement::AgreementLedger` at `<store-root>/.ferry/agreement/
<folder>-<peer>.agree` — byte-exact serialization per docs/store-format.md
§Last-agreed manifest pointer, the format T-010 reads. The M0 convenience
record (with full manifest bytes for offline baseline recovery) is kept
alongside. Interop tests assert both sides' records parse to equal
peer/manifest ids.

**Integrity.** Packs verify BLAKE3(ciphertext)==name before insertion;
blobs verify after decrypt before any store write; gaps re-request up to
budget then fail cleanly (`MissingItems`); AEAD tag failures count into
`EngineStats.rejected_items` (a tampered sealed frame dies at its tag
before item checks can run) and fail the session without recording
agreement. CorruptingTransport mechanically retargeted to the first post-
handshake inbound frame (sealed frames hide message tags); scenario
assertions untouched.

**Verification.** Workspace `cargo test --workspace`: 435 passed / 0 failed
across 39 targets (ferry-sync: 38 incl. 5 new protocol_v1 interop/policy/
legacy tests). Clippy `--workspace --all-targets`: zero warnings. `cargo
fmt --all --check` clean. Suite also green under
`FERRY_SYNC_E2E_TRANSPORT=iroh`. `skeleton-e2e.sh` exit 0 in both modes
(tcp converged ≤2s of its window, iroh ≤1s) with new assertions proving
`encrypted=yes` on both daemons and failing on any `encrypted=no`.

**Deviations, reported honestly.**
- Over the `Transport` seam the spec's u32 BE length prefix is represented
  by the transport's own frame boundary (the TCP impl prefixes LE); AEAD
  still binds each frame's length, and literal BE wire bytes are produced
  by `RawLink` — exactly what the cross-engine interop tests drive.
- Stage-end markers: docs/store-format.md says the server answers the
  empty REQUEST_ITEMS marker with a bare empty ITEM_BATCH, but the frozen
  reference engine returns silently; we match the reference (interop is
  authoritative). Doc fix belongs to T-008's owner.
- ferry-proto was NOT modified (zero diffs); no additive helpers needed.
