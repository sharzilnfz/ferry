# T-002: Chunking and blob store

Status: done
Depends on: T-001

Implement the store core per T-001 spec: CDC chunking (evaluate restic/chunker
algorithm lineage vs a Rust FastCDC crate; benchmark both on text and binary
fixtures), content-addressed blob read/write with atomic creation, pack files
with randomized membership, GC of unreferenced blobs behind a grace period
(idea1's grace-period pattern applies). Encryption hooks stubbed as pass-through
until T-007/T-008 wire real keys.

Acceptance: round-trip property tests (arbitrary bytes → chunks → blobs →
reassembly identical); dedup demonstrated for shifted insertions; concurrent
writers safe.

## Comments

Done in workspace `crates/ferry-store` (first crate of the `ferry-sync`
workspace; layout mirrors idea1's). 74 unit tests green;
`cargo test --workspace` and `cargo clippy --workspace --all-targets` are clean,
`cargo fmt` applied. Dependencies: blake3, rand, hkdf, sha2, thiserror,
unicode-normalization (+ tempfile as dev-dep) — all MIT/Apache-2.0 dual.

Chunker evaluation collapsed to the spec-normative algorithm. The ticket's
benchmark mandate (restic/chunker lineage vs a Rust FastCDC crate) is mooted by
T-001: `docs/store-format.md` freezes the Rabin-fingerprint scheme byte for
byte (64-byte window, MIN 512 KiB / SPLIT low 20 bits / MAX 8 MiB, degree-53
monic irreducible per-folder polynomial, x^504 slide-out), so there is nothing
left to choose at this layer — any alternative chunker would break the
compatibility contract. FastCDC stays a v2 option only via a format-version
bump. Correctness evidence instead of benchmarks: gf ops cross-checked against
brute-force references, Rabin irreducibility validated against exhaustive
trial division on all monic degree-2/3/5/7 polynomials, x^n computed two ways
per spec, plus a pinned regression test proving two different prefixes share
identical boundaries across a common 4 MiB suffix. That test caught a real bug
during development: the slide-out table skipped its final GF(2) reduction,
which silently destroyed window locality (and therefore all dedup).

GC reconciliation with T-001: the store-format spec defers pruning ("Packs
are immutable after rename. Pruning removes whole packs."), while this ticket
requires GC. Reconciled exactly along that line: GC deletes WHOLE PACKS whose
every blob is unreachable from caller-designated live manifests, and only
after the pack has been continuously unreferenced longer than a configurable
grace period (default guidance 48h prod, seconds-scale tests; injected clock).
Never deletes referenced data; packs containing a polynomial record are always
live; unverifiable packs are reported, never deleted. First-unreferenced
timestamps persist in `.ferry/gc-state` (implementation-local, deletable —
losing it only resets grace clocks) so the protection survives restarts.
Cross-process racing of GC against writers remains accepted v0 residual risk;
in-process writers/GC share mutexes.

Crypto seam: `trait PackCipher` sits exactly where the spec's STREAM segments
live (`seal/open` one segment given key/nonce/aad). `PassthroughCipher` emits
correctly framed ciphertext (payload + zeroed 16-byte tag slot) so every
offset, length, footer_len, pack name, and body-region identity in the store
is already spec-conformant; it provides NO secrecy/authenticity and must not
ship past T-007/T-008. Key schedule is fully implemented and tested now:
HKDF-SHA-256 pack keys (info "ferry/v1/pack/{data,meta}") and index keys
("ferry/v1/index") verified against RFC 5869 vectors A.1/A.3 plus a pinned
known-answer value; nonce construction ((counter<<1|last_flag) big-endian,
footer reserved counter FFFFFFFF) and AAD (header||kind||role) have exact-byte
unit tests. Swapping the real ChaCha20-Poly1305 in touches only `crypto.rs`.

Deferred (belongs to other tickets): CONFIG_HEAD container + FMK wrapping and
pairing ritual (T-007); snapshot/diff APIs over the manifest serialization
added here (T-003); scan/watch integration that will drive the chunker (T-004);
index-file compaction (old `.ferryindex` files accumulate until then — union
reads tolerate them by design); Windows directory fsync is best-effort no-op.
