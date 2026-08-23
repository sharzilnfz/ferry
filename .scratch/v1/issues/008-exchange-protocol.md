# T-008: Encrypted exchange protocol

Status: done
Depends on: T-002, T-003, T-007

Wire protocol over any byte stream: hello/authenticate (device keys), offer
manifests, request missing blobs by hash, stream encrypted chunks, verify
every block hash after decryption before it touches disk. Protocol messages
documented as an extension of `docs/store-format.md`. Version-negotiated from
day one.

Acceptance: T-006's skeleton runs over this protocol with encryption on;
corrupted chunks in transit are detected and rejected, never written.

## Comments

### Delivered

New crate `crates/ferry-proto` (framing, negotiation, handshake, engine,
agreement ledger) plus a normative "Wire protocol v1" section in
`docs/store-format.md` (lines ~621-904). Loopback acceptance harness in
`crates/ferry-proto/tests/acceptance.rs`; protocol-adversary tests in-crate
(`src/engine_tests.rs`) where they need internals.

### Auth scheme choice + rationale

Noise-style IMPLICIT authentication, no signatures/challenge round trip.
After Hello/HelloAck both sides compute three X25519 secrets:
e1 = eph_I×eph_R (fresh per connection → forward secrecy), m1 = stat_I×eph_R,
m2 = stat_R×eph_I. HKDF-SHA256 two-stage schedule salted by the BLAKE3
transcript hash yields single-use auth keys and per-direction traffic keys.
Each side seals exactly ONE message (its device_id) under its auth key; a
valid Poly1305 tag is unforgeable without the static secret, so the tag IS
the proof-of-possession. Chose this over "seal challenge to peer's pub key"
because one KDF tree serves auth AND traffic, the transcript hash binds every
handshake byte into the proof, and replay dies structurally (fresh
ephemerals → unique transcript per connection → replayed AUTH fails its tag).
Session keys are ephemeral-per-connection, forward-secret by construction.
Post-auth frames: ChaCha20-Poly1305, nonce = "FPN1" || u64 BE per-direction
sequence, length prefix bound as AAD. Rekey not needed for v1 (u64 counter
ceiling documented; exhaustion is a hard typed error, never nonce reuse).

### Message inventory (wire v1)

HELLO(0x01) HELLO_ACK(0x02, carries max + chosen agreed version)
AUTH_INIT(0x03) AUTH_CONFIRM(0x04) FOLDER_OFFER(0x05)
INDEX_ADVERT(0x06, index-table rows + continuation flag)
REQUEST_ITEMS(0x07, empty = end-of-pull-stage marker)
REQUEST_PACKS(0x08) ITEM_BATCH(0x09, empty batch terminates every response)
PACK_ITEM(0x0A, whole pack ciphertext under BLAKE3 name) BYE(0x0B, reason
codes 0-5). Ticket's "AuthChallenge/AuthResponse" folded into the four-
message handshake above — the nonces ride in Hello/HelloAck and the proofs
are the AUTH messages.

### Negotiation rule (as implemented)

Majors must match (else BYE(1)); session version = min of maxima within the
major; unknown type pre-auth = violation; post-auth unknown type is SKIPPED
iff sender advertised a higher minor AND flag bits we don't know
(skip-if-flagged), else violation → BYE(2). Skippable frames must still
authenticate.

### Verification-after-decryption

Every received blob: BLAKE3(plaintext) == claimed id before store.put_blob.
Packs: BLAKE3(ciphertext) == name before any disk write or decryption
(ingest via temp+rename+rebuild_index). Corrupt/wrong-id items: typed error,
never written, re-requested up to retry budget, then clean MissingItems
failure.

### Agreement bookkeeping

ferry-store had NO record type for last-agreed pointers, so ferry-proto
implements read/write of the canonical serialization from docs/
store-format.md ("Last-agreed manifest pointer", 77 bytes: peer32 manifest32
sec i64 nsec u32 flags u8=0) in `agreement.rs`, stored under
`<store>/agreement/<folder>-<peer>.agree`, atomic temp+rename. Recorded when
round-2 offers show equal nonzero manifests on both sides.

### ferry-store seam needed (reporting per ticket rules)

ONE additive public accessor: `Store::index_entries()` (read-only snapshot
of the location table). Forced because the format doc itself prescribes
"index entries become advertisement entries" and rebuilding that view from
pack footers outside the crate would duplicate format logic. No behavior
changes elsewhere in ferry-store; no other crate touched. ferry-sync NOT
touched (out of scope per ticket).

### Integration-with-T-006 note

T-006's skeleton could not run here directly (its branch is unmerged);
per-ticket fallback implemented instead: full loopback harness proves
convergence with ENCRYPTION ON over real localhost TCP (plus duplex pair,
plus plaintext variants): device A snapshots a 3 MiB multi-chunk file +
deep unicode/emoji directory tree, device B ends with identical manifest id,
all blobs present and hash-verified, content materialized to disk via
throwaway temp+rename dump and byte-compared against the source. Final
skeleton integration lands post-merge of T-006: swap this engine's
`run_engine` in behind T-006's sync loop and replace the throwaway
materializer with T-005/T-006's production one.

### Known limitations (documented, deliberate)

- Lockstep conversation assumes each side's advert sequence for one folder
  fits socket buffers per turn; chunked/streamed adverts are future-minor.
- Pack ingest re-runs rebuild_index per pack (O(packs) each); fine at v0
  scale, batch it later.
- Divergent-manifest sessions do a fetch-only union (both stores gain the
  peer's objects); pointer reconciliation stays T-006/T-010 scope, so no
  agreement is recorded for divergent case (tests pin this).
