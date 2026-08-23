# Store format spec v1

Status: normative for v0. Version 1, frozen 2026-08-24.
Owner ticket: T-001. Blocks T-002 (store core), T-003 (scan/materialize),
T-008 (transport).

This document is the compatibility contract between Ferry versions and
between independent implementations (ADR-0001). Given identical inputs, two
conforming implementations produce byte-identical serializations for every
structure defined here, with one deliberate exception: pack membership is
randomized and recorded, never derived (see [Pack files](#pack-files)).

The bar to meet: an engineer can implement a compatible store in another
language from this document alone. Every constant, byte layout, integer
width, endianness, normalization rule, and algorithm step is specified. Where
a mechanism is borrowed from prior art, the source is named; deviations are
listed with a one-line reason in the [prior art](#prior-art-cross-check)
section.

Conformance words (MUST, SHOULD, MAY) follow RFC 2119.

## Vocabulary

Terms come from `CONTEXT.md`: store, blob, manifest, tree, folder,
materialize/hydrate, pairing, relay. Two more used throughout:

- **Chunk**: one CDC cut of a file's plaintext bytes. Chunks are blobs.
- **Blob kind**: what a serialized object is. Data chunk, tree node, or
  manifest. Trees and manifests are metadata blobs.

## Conventions

All multi-byte integers are **little-endian** unless stated otherwise. The
one exception is the 4-byte nonce counter word, which is big-endian to match
the age STREAM construction we mirror; this is called out again where it
appears. All hashes are raw 32-byte values on the wire. Hex display form is
lowercase. Byte offsets and lengths are unsigned unless marked `i64`.

Strings carried in structures are UTF-8 in **Unicode NFC** normalization,
mandatory regardless of platform, borrowed from the Syncthing BEP spec. The
path separator in stored names is `/`, always. Stored names are single path
components: they MUST NOT contain `/`, NUL, and MUST NOT be `.` or `..`.
Names are compared as raw bytes. Two names differing only in case are
distinct and both may exist; detecting collisions on case-insensitive hosts
is the materializer's job (SPEC guardrails), not the format's.

## File header

Every standalone Ferry container file starts with a 10-byte header:

```
offset  size  field
0       5     magic, ASCII "FERRY" (bytes 46 45 52 52 59)
5       1     kind (u8)
6       4     format_version (u32 LE), currently 1
```

Kind values:

| value | kind         | contents                                   |
|-------|--------------|--------------------------------------------|
| 0x01  | PACK_DATA    | pack file holding data chunks               |
| 0x02  | PACK_META    | pack file holding tree nodes and manifests |
| 0x03  | INDEX        | encrypted blob-location table              |
| 0x04  | CONFIG_HEAD  | plaintext folder bootstrap record          |

Structures serialized *inside* packs (chunks, tree nodes, manifests, the
polynomial record, index tables) do not repeat the header. They are governed
by the container's `format_version`. There are no per-structure versions.

Readers MUST reject a file whose magic, kind, or version they do not know.
No guessing, no best-effort parse. This mirrors restic's behavior on unknown
repository versions. Writers MUST write version 1.

Forward compatibility policy: new container kinds may be added without
bumping the version if all previously defined layouts stay unchanged.
Reserved fields exist in some structures; writers MUST write zeros, readers
MUST reject nonzero values in fields marked reserved for v1. Any change to an
existing layout bumps `format_version` and old readers refuse the new file.

## Hashing and addressing

**Hash function: BLAKE3, 256-bit output (32 bytes), unkeyed mode.**

Chunks, tree nodes, and manifests are addressed by `BLAKE3(plaintext bytes)`.
A receiver verifies after decryption: decrypt the blob, hash the plaintext,
require equality with the address it asked for. This follows restic, whose
pack headers and index store plaintext hashes precisely so integrity survives
encryption.

Pack FILES are addressed by `BLAKE3(entire pack ciphertext bytes)`, the same
rule restic uses for repository files. The name is verified before any
decryption happens, so corrupted or tampered packs are rejected without key
material touching them. The name leaks nothing about plaintext.

Rationale for BLAKE3 over SHA-256: scanning is the hot path of this tool.
Every changed byte is hashed on every scan, agents churn thousands of files,
and scan throughput is listed as the top risk in SPEC.md. BLAKE3's
SIMD-parallel design gives multi-GB/s hashing on ordinary hardware, several
times faster than SHA-256 even with SHA-NI, and its single well-specified
construction has maintained implementations in Rust, Go, Python, C, and
JavaScript, so the independent-implementation bar stays reachable. Both give
256-bit collision resistance; dedup correctness needs no more.

Addresses MUST NOT be keyed. Keyed addressing would break cross-machine
deduplication and manifest comparison, since keys differ per folder.

## Chunking

Content-defined chunking from the restic/chunker lineage: a Rabin fingerprint
over a sliding 64-byte window decides cut points, so inserting bytes shifts
only nearby boundaries and later chunks stay stable across versions of a
file.

### Parameters (v1 constants)

```
WINDOW_SIZE = 64 bytes
MIN_SIZE    = 524288      (512 KiB)
AVG_BITS    = 20
SPLIT_MASK  = (1 << AVG_BITS) - 1 = 1048575   (cut when low 20 bits are zero)
MAX_SIZE    = 8388608     (8 MiB)
POLY_DEGREE = 53
```

These sit inside the 64 KiB-to-8 MiB envelope ADR-0005 sets. Files smaller
than MIN_SIZE are never split: a 10 KB file is one 10 KB chunk. An empty file
has zero chunks. Small-file granularity is therefore unaffected by MIN_SIZE;
MIN only suppresses cuts inside larger streams.

### The chunker polynomial

Each folder has one random monic irreducible polynomial of degree 53 over
GF(2), generated once at `ferry init` and reused for every chunking
decision in that folder, following restic's per-repository polynomial. The
polynomial is secret from storage observers (see where it lives under
[Folder layout](#folder-layout)).

Representation: a u64 bitfield, bit i set means the coefficient of x^i is 1.
Bit 53 MUST be set (monic). Bits 54..63 MUST be zero. Display form is the
hex of the u64, e.g. restic configs show values like `25b468838dcb75`.

Arithmetic is carryless (XOR is addition and subtraction):

- `gf_mul(a, b)` = schoolbook carryless multiply: for each set bit i of b,
  XOR `a << i` into the accumulator.
- `gf_mod(v, p)` = polynomial long division remainder: while degree(v) >= 53,
  XOR `p << (degree(v) - 53)` into v.
- `gf_pow_x(n, p)` = x^n mod p, computed by n doublings (`v = gf_mod(v << 1)`)
  or square-and-multiply; result must match either way.

Generation procedure (runs at folder creation):

1. Draw 53 random bits from the OS CSPRNG, set bit 53, giving candidate p.
2. Test irreducibility with the Rabin test. 53 is prime, so p is irreducible
   iff both hold:
   a. `gf_pow_x(2, p)` squared 53 times equals x: compute g = x^(2^53) mod p
      by repeated squaring and require g == x.
   b. `gcd(p, x^2 + x) == 1`, equivalently p has a nonzero constant term and
      an odd number of set bits.
3. If either fails, draw again. Expected iterations: roughly 53/2, since
   about half of all degree-53 polynomials are irreducible.

Storage: the u64 is serialized into the encrypted polynomial record defined
under [Folder layout](#folder-layout). Losing the polynomial loses the ability
to reproduce chunk boundaries, so the folder becomes unreadable-by-chunker
even though blobs remain decryptable. It is protected like a key.

### Algorithm

Given input bytes and initialized state (empty 64-byte window `win`,
window position `wpos = 0`, fingerprint `fp = 0`, count `filled = 0`):

Append byte b:

```
if filled < 64:
    win[wpos] = b
    wpos = (wpos + 1) mod 64
    filled = filled + 1
    fp = gf_mod((fp << 8) | b, p)
else:
    out = win[wpos]              # outgoing oldest byte
    win[wpos] = b
    wpos = (wpos + 1) mod 64
    fp  = fp XOR gf_mul(gf_pow_x(504, p), out)   # remove out * x^504
    fp  = gf_mod((fp << 8) | b, p)
```

The x^504 term: after 64 bytes accumulate, the oldest byte has been shifted
left by 8 bits 63 times, so dropping it subtracts `out * x^(8*63)` from the
fingerprint before shifting in the new byte. Implementations SHOULD
precompute `out_table[out] = gf_mul(out, gf_pow_x(504, p))` once per folder
and a reduction table for `gf_mod((fp << 8) | b)`, as restic/chunker does.
Any table scheme is conformant if results match the formulas exactly.

Cutting loop for one file, processing byte by byte and tracking current
length `len`:

```
len = 0
for each byte b in file:
    append(b); len = len + 1
    if len >= MIN_SIZE and (fp & SPLIT_MASK) == 0:
        emit chunk of len bytes        # natural cut, byte included
        reset state; len = 0           # fp=0, filled=0, wpos=0, window cleared
    elif len == MAX_SIZE:
        emit chunk of len bytes        # forced cut at MAX_SIZE
        reset state; len = 0
after EOF:
    if len > 0: emit chunk of len bytes
```

Clamping order matters: the minimum clamp gates whether the split test runs
at all, the maximum clamp fires only if no natural cut happened first. A
natural cut and the max clamp never compete at the same byte because the
split test is evaluated before the max test. State resets completely between
chunks; fingerprints do not carry across boundaries.

## Pack files

Packs group blobs so encryption happens above individual chunk boundaries
(ADR-0005). One pack is one immutable file, written once, never modified.

### Layout

```
+---------------------------------------------+
| file header (10 bytes, kind PACK_DATA/PACK_META) |
| pack_salt (16 random bytes)                 |
| body: STREAM segments (encrypted)           |
| footer ciphertext                           |
| footer_len (u32 LE: length of footer ciphertext) |
+---------------------------------------------+
```

Body plaintext is the concatenation of all contained blob plaintexts, back
to back, no padding, no inline framing. The footer maps everything.

Footer plaintext:

```
u64 LE body_plain_len      # total plaintext bytes in body
u32 LE blob_count
per blob, in body order:
    u8  blob_kind          # 0x01 data chunk, 0x02 tree node, 0x03 manifest,
                           # 0x04 polynomial record
    32B id                 # BLAKE3 of this blob's plaintext
    u64 LE plain_off       # offset within reassembled body plaintext
    u64 LE plain_len
u32 LE reserved (zeros)
```

Data chunks go only in PACK_DATA. Tree nodes, manifests, and the polynomial
record go only in PACK_META. Never mixed, like restic's data/tree separation.

### Encryption envelope

Age-STREAM-style chunked AEAD, using RFC 8439 ChaCha20-Poly1305.

Cipher choice: ChaCha20-Poly1305 over AES-256-GCM. Every Ferry peer is also
an endpoint on consumer hardware, including older Intel Macs and ARM boards
without AES acceleration, where software ChaCha20 holds a large constant-time
advantage over software AES. age chose ChaCha20-Poly1305 for the same reason,
which keeps audited reference constructions to compare against. Security
margin is equivalent at these key sizes.

Key schedule. Each pack derives its own key so that RNG failure affecting
one salt cannot cause cross-pack nonce reuse:

```
FMK       = 32-byte per-folder master key (random at folder creation)
pack_salt = the 16 bytes from the pack prologue
pack_key  = HKDF-SHA-256(ikm = FMK,
                         salt = pack_salt,
                         info = "ferry/v1/pack/data" for PACK_DATA
                                "ferry/v1/pack/meta" for PACK_META,
                         L = 32)
```

Body segmentation: the body plaintext is split into consecutive segments of
65536 plaintext bytes; the last segment may be shorter. The body region of
the file runs from offset 26 to `filesize - 4 - footer_len`. Given
`body_plain_len` from the footer, the segment count is
`(body_plain_len + 65535) / 65536` (integer division), and a conforming file
must satisfy `body_region_len == body_plain_len + 16 * segment_count`;
readers SHOULD verify this. Each segment is sealed independently:

```
counter    = segment index, 0-based
last_flag  = 0x00 for all body segments except the final one, which is 0x01
nonce      = 8 zero bytes || u32 BIG-ENDIAN ((counter << 1) | last_flag)
             # 12 bytes total; counter occupies 31 bits, flag the low bit
aad        = file header (10 bytes) || container kind byte || role byte
             # role: 0x00 body, 0x01 footer
ciphertext = ChaCha20-Poly1305_seal(pack_key, nonce, aad, segment_plaintext)
```

Segment ciphertext length is plaintext length + 16 (the Poly1305 tag).
Segments concatenate in the body region with no separators. Individual
segments decrypt and authenticate independently given the salt, so a reader
can seek: see [Reading a blob](#reading-a-blob).

The footer is sealed with the same pack_key and aad, with a reserved counter
value so it cannot collide with body counters:

```
footer_nonce = 8 zero bytes || FF FF FF FF
             # equals ((0x7FFFFFFF << 1) | 1); body counters never reach 2^30,
             # which would take 64 TiB of plaintext in one pack
last_flag    = 1 (embedded in the nonce word as shown)
```

The trailing `footer_len` u32 LE is stored in clear, restic's convention,
letting a reader find the footer by reading the final four bytes. Because the
body plaintext length sits inside the authenticated footer, an attacker
cannot truncate the body without breaking the footer's tag, and cannot alter
`footer_len` meaningfully without producing undecryptable garbage.

What observers see: pack sizes and segment counts. Not chunk counts, not
individual chunk sizes, not tree shapes, not names. Continuous-stream
encryption is why this spec needs no per-blob IVs: restic's `IV || CT || MAC`
per blob exposes each blob's length on disk, the leakage ADR-0005 worries
about; folding all blobs into one authenticated stream hides lengths entirely.

### Creating a pack atomically

1. Accumulate staged blobs (see membership rules below).
2. Serialize body plaintext, pick 16 fresh salt bytes from the CSPRNG.
3. Write to `<store>/tmp/pack-<16 random hex bytes>.tmp`: prologue, then all
   body segment ciphertexts in order, then footer ciphertext, then the u32
   LE footer ciphertext length.
4. Compute `name = hex(BLAKE3(every byte just written))`.
5. fsync the file. Rename to `<store>/packs/<name>.pack`. fsync the packs
   directory.
6. On any failure, delete the temp file and abort; a half-written temp file
   is invisible to readers, which only ever look in `packs/`.

Packs are immutable after rename. Pruning (future) removes whole packs.

### Membership randomization

Which chunks land in which pack is deliberately unpredictable, the mitigation
restic 0.18 adopted against chunk-length fingerprinting (their PR #5295,
called for by ADR-0005). With stream encryption hiding sizes anyway,
randomization is belt and suspenders: it also defeats correlating pack
growth patterns over time.

Rules:

- The writer keeps up to W = 8 concurrently open staging packs per kind
  (data, meta), each targeting a sealed size of 16 MiB.
- Each newly produced blob is assigned to one of the open staging packs of
  its kind, drawn uniformly with the OS CSPRNG.
- If assignment would push the staging pack past its 16 MiB target, seal that
  pack immediately (emit per above) and redraw among the remaining open packs.
  If none remain open, start a new one.
- When a scan burst ends, seal all staging packs, even tiny ones. Empty
  staging packs are discarded without writing.
- Randomness is never persisted and never reproduced. The resulting layout is
  recorded where it matters: blob positions live in pack footers, and the
  index records chunk-to-pack mappings. Consumers MUST NOT assume any
  relationship between blob order in a pack and file order in a tree.

## Index

The index maps blob id to location: `(id) -> (pack, plain_off, plain_len)`,
plus the blob kind. Transport (T-008) uses it to answer "do you have blob X";
materialization uses it to fetch and decrypt.

An index file is a container of kind INDEX built from exactly the same
envelope rules as a pack, with an empty body:

```
file header (10 bytes, kind INDEX)
index_salt (16 random bytes)
index table ciphertext   # the table below, sealed like a pack footer
table_len (u32 LE: length of the ciphertext)
```

The table is encrypted with the pack-footer rule: key =
`HKDF-SHA-256(FMK, salt = index_salt, info = "ferry/v1/index", L = 32)`,
nonce = 8 zero bytes || FF FF FF FF, aad = file header || kind byte (0x03)
|| role byte 0x01. Table plaintext:

```
u32 LE entry_count
entries sorted ascending by (blob_kind, id bytes):
    u8  blob_kind     # same values as pack footers
    32B id
    32B pack_id       # raw 32-byte BLAKE3 of the pack's ciphertext
    u64 LE plain_off
    u64 LE plain_len
```

Sortedness makes the table searchable and its serialization deterministic.
Multiple index files may coexist; their union is the index. Writers append a
fresh index file rather than rewriting in place. Readers resolve duplicates
by preferring any entry whose pack still exists.

Recovery path: the index is derivable. Scan every `*.pack` file, verify each
against its filename hash, decrypt footers, rebuild entries. Implementations
SHOULD offer this as `rebuild` behavior, as restic does.

Index files are encrypted because they leak the social graph of your data:
which blobs exist, how many, how big. Same threat model as packs.

## Manifest schema

A snapshot of one directory tree. Two object types, both serialized
deterministically and addressed by `BLAKE3(serialized plaintext)`:

- **Tree node**: the listing of one directory.
- **Root manifest**: points at the root tree node and carries lineage.

Determinism rules, binding on all serializers:

- Field order is exactly as specified. No tags, no optional-field markers,
  no padding, no alignment.
- Integers are fixed-width, little-endian, per the tables.
- Entries are sorted by their NFC-encoded name BYTES, ascending byte order.
- Duplicate names within one tree node are invalid; reject the object.
- Reserved fields are zeros.

### Tree node

```
u32 LE entry_count
per entry, sorted by name bytes:
    u8  entry_type        # 0x00 file, 0x01 dir, 0x02 symlink
    u32 LE name_len       # byte length of NFC UTF-8 name
    name                  # single component, rules under Conventions
    u8  flags             # bit 0 = executable; valid on files only, else 0
                          # bits 1..7 reserved zeros
    i64 LE mtime_sec      # Unix epoch seconds, signed
    u32 LE mtime_nsec     # 0 .. 999_999_999, always normalized non-negative
    -- file --
    u64 LE size           # logical plaintext size == sum of chunk lengths
    u32 LE chunk_count
    chunk_count x:
        32B chunk_id      # BLAKE3 of chunk plaintext
        u64 LE chunk_len  # chunk plaintext length, ordered sequence
    -- dir --
    32B child_tree_id     # BLAKE3 of the child directory's tree node
    -- symlink --
    u32 LE target_len
    target                # NFC UTF-8, stored verbatim otherwise
```

Notes:

- Mode is reduced to the exec bit, per the SPEC permission-subset guardrail.
  Everything else (uid, gid, rwx groups, setuid) is intentionally dropped;
  syncing it cannot survive Windows round trips and Resilio's own support
  concedes the point.
- The mtime pair follows BEP's `modified_s` / `modified_ns` split. Negative
  seconds with positive nanos, timespec style, represent pre-1970 times.
- The chunk list is ORDERED. Order is the file's content definition.
- Symlink targets MUST decode as valid UTF-8 after NFC normalization. Targets
  that cannot are refused loudly at scan time. This drops restic's
  `linktarget_raw` escape hatch; see deviations.
- Tree nodes are hash-addressed and therefore deduplicated automatically:
  two directories with identical listings anywhere in the tree, or across
  snapshots, serialize identically and store once. Same trick as restic's
  trees, minus JSON.

### Root manifest

```
16B folder_id          # UUIDv4, raw bytes, generated at ferry init
32B device_id          # creating device's X25519 public key, raw
i64 LE created_sec
u32 LE created_nsec
32B root_tree_id       # BLAKE3 of the root tree node
32B parent_manifest_id # previous agreed manifest; 32 zero bytes if none
32B reserved           # zeros in v1
```

`parent_manifest_id` gives every snapshot a linear ancestry chain per device.
It is provenance, not authority: the authority for reconciliation is the
last-agreed pointer below, which peers track per folder per peer (ADR-0004).

Manifests are metadata blobs: they ride in PACK_META, are indexed like other
blobs, and are encrypted like everything else. Names, tree shapes, and
lineage are invisible to relays and untrusted disks (ADR-0002).

## Last-agreed manifest pointer

Part of tracked local state per ADR-0004. After a sync cycle concludes and
both devices hold identical manifests for a folder, each device records, for
that folder and for each peer involved:

```
32B peer_device_id    # the peer's X25519 public key
32B manifest_id       # the manifest both sides agreed on
i64 LE agreed_sec     # local wall clock when agreement was recorded
u32 LE agreed_nsec
u8  flags             # 0 in v1
```

This is the base state for three-way reconciliation: local tree, remote tree,
and last-agreed manifest as ancestor. Divergence from the ancestor decides
conflicts; the timestamp is advisory (display, tie-break heuristics), never a
correctness input.

The record is local state. It is not transmitted verbatim; peers re-derive
agreement by exchanging manifests themselves. Its canonical serialization is
specified so the local database stays compatible across versions and so a
future protocol (offline conflict negotiation) can lift it onto the wire
unchanged.

## Folder layout

On-disk layout under the synced folder root. Names of directories are part
of the contract; the store lives in one place so backup tools and users can
find it.

```
<folder>/.ferry/
    config                      # CONFIG_HEAD container, plaintext (below)
    packs/<hex-name>.pack       # immutable packs, both kinds
    index/<n>.ferryindex        # INDEX containers
    tmp/                        # staging area for atomic writes
```

CONFIG_HEAD plaintext body (this container is NOT encrypted; it contains no
secret, only public identifiers and key-wrapping ciphertexts):

```
16B folder_id
u32 LE reserved (zeros)
u32 LE wrapped_key_count
per wrapped key:
    32B device_x25519_pub   # recipient device
    u32 LE wrapped_len
    wrapped                 # envelope sketch below
```

The chunker polynomial is too sensitive for a plaintext file (an observer
with the polynomial plus any chunk-size oracle could confirm known files are
present, the exact attack class in ADR-0005), so it ships as an encrypted
blob: blob_kind 0x04, plaintext `u64 LE polynomial`, stored in a PACK_META
like any other blob and found through the index.

Bootstrap sequence for a reader opening a folder:

1. Parse `config` (validate magic/kind/version, folder_id).
2. Unwrap the FMK using this device's identity key (envelope sketch below).
3. Load indexes, locate the polynomial blob, decrypt, initialize the chunker.
4. From here the store behaves like any other pack set.

### Key envelope sketch (normative shape, T-007 owns the ritual)

FMK wrapping follows age's X25519 recipient pattern:

```
ephemeral_secret = X25519 scalar, fresh per wrap
shared = X25519(ephemeral_secret, device_x25519_pub)
wrap_key = HKDF-SHA-256(ikm = shared,
                        salt = ephemeral_pub || device_x25519_pub,
                        info = "ferry/v1/keywrap", L = 32)
wrapped = ephemeral_pub (32B) || ChaCha20-Poly1305_seal(wrap_key, 12 zero
          bytes, aad = folder_id, plaintext = FMK)
# 32 + 48 = 80 bytes; wrapped_len MUST be 80 in v1
```

Pairing, QR codes, short codes, and revocation are T-007's scope. The format
only fixes the wire-visible shape above. Losing all devices loses all data;
that tradeoff is ADR-0002's, stated here so implementers do not invent a
recovery side door.

## Reading a blob

Normative procedure, written out because seek-across-segments is where
independent implementations drift:

1. Look up `(pack_id, plain_off, plain_len)` in the index.
2. Open `packs/<hex pack_id>.pack`. Verify `BLAKE3(file bytes) == pack_id`.
   Reject on mismatch without decrypting.
3. Parse the 10-byte header, read `pack_salt`, read trailing u32 LE
   `footer_len`, read and decrypt the footer (reserved counter, aad as
   specified). Confirm the blob's id, `plain_off`, and `plain_len` appear in
   the footer table. If the index and footer disagree, trust neither and stop.
4. `first_seg = plain_off / 65536` (floor), `last_seg = (plain_off +
   plain_len - 1) / 65536` (floor; plain_len > 0 always).
5. For each segment s in that range, counting from 0 at the start of the
   body region: read its ciphertext, decrypt with counter s and the correct
   last_flag (0x01 only when s is the final body segment,
   `s == (body_plain_len + 65535) / 65536 - 1`), verify the tag.
   Authentication failure aborts the whole read.
6. Concatenate the decrypted segments, slice `[plain_off mod 65536 : ... +
   plain_len]` from the concatenation, hash the result, require
   `BLAKE3(result) == id`. Only now is the plaintext trusted.

Step 6 is the verify-after-decrypt rule: addresses are plaintext hashes, and
no caller sees plaintext that has not matched its address.

## Wire note

T-008 defines message framing, handshake, advertisement, and request/response
flow. Those messages REUSE the serializations in this document byte for byte:
tree nodes and root manifests move exactly as stored (still encrypted under
the folder key, so relays stay blind per ADR-0002/0003), index entries become
advertisement entries, and whole packs transfer as units named by ciphertext
hash so end-to-end integrity checking costs nothing extra. Nothing in this
document should be duplicated or re-encoded at the transport layer. Framing
details, chunked transfer resume, and pull negotiation are out of scope here.

## Prior art cross-check

Fetched and checked against both sources on 2026-08-24. restic's design page
lives at `100_references.html` today; the `design.html` URL cited in the
ticket and in `research/landscape.md` resolves to the References section.

Borrowed from restic ([design references](https://restic.readthedocs.io/en/stable/100_references.html)):

- Content-addressed, write-once objects; filenames derived from content
  hashes; atomic writes; index rebuildable from headers.
- Pack files with an encrypted trailing header and a cleartext u32 LE header
  length at the end of the file.
- Separating data packs from metadata packs (their data/tree split, ours
  data/meta).
- A random irreducible polynomial per repository, generated at init and
  stored in config; Rabin-fingerprint CDC with a 64-byte window and min/avg/
  max clamping near our constants (theirs: 512 KiB / 1 MiB / 8 MiB).
- Randomized chunk-to-pack assignment as the chunk-leakage mitigation
  (restic 0.18, PR #5295), which ADR-0005 names directly.
- Version check and abort on unknown repository version.
- Deterministic encoding of trees so dedup works (theirs: Go json
  determinism; ours: fixed binary layout).

Borrowed from the Syncthing BEP v1 spec
([bep-v1](https://docs.syncthing.net/specs/bep-v1.html)):

- NFC normalization for every name regardless of platform, and `/` as the
  separator.
- Modification time as a seconds-plus-nanoseconds pair.
- Verify every received piece against its declared hash before applying it.
- Metadata first, data second: exchange indexes (for us, manifests) before
  requesting blocks, the shape of BEP's Index/Request/Response cycle.
- Temp-file-then-atomic-rename as the universal write discipline (their
  `.syncthing.<name>.tmp` pattern applied to our packs and, in T-002, to
  materialized files).

Deviations, one line each:

- BLAKE3 instead of SHA-256: scanning is the throughput bottleneck (SPEC risk
  list), and BLAKE3 buys multiplicative headroom there.
- Whole-pack continuous STREAM encryption instead of restic's per-blob
  `IV || CT || MAC`: per-blob envelopes expose individual chunk lengths on
  untrusted storage, the leakage ADR-0005 exists to prevent; one stream hides
  them outright, and membership randomization remains as defense in depth.
- No version vectors, sequence numbers, or `modified_by` (BEP carries all
  three): reconciliation is Mutagen-style three-way against the last-agreed
  manifest per ADR-0004, and conflicts quarantine instead of merging, so
  vector clocks buy nothing.
- No per-file block-size ladder (BEP negotiates power-of-two block sizes per
  file): CDC yields variable chunks, one global parameter set replaces
  per-file negotiation.
- Fixed binary serialization instead of restic's JSON-determinism approach:
  restic requires matching Go's `encoding/json` behavior, which is hostile to
  independent implementations; fixed field order needs no library consensus.
- Symlink targets must be valid NFC UTF-8, no raw-bytes escape hatch (restic
  added `linktarget_raw`): dev-directory scope, loud refusal beats silent
  mojibake, and a raw variant can arrive as a v2 addition.
