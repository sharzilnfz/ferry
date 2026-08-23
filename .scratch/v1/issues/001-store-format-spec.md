# T-001: Store format spec v1

Status: done
Blocks: T-002, T-003, T-008

Write `docs/store-format.md`: the versioned on-disk and wire contract.
Blob addressing (BLAKE3 or SHA-256 of ciphertext vs plaintext — decide and
document), CDC parameters (min/avg/max, per-folder polynomial), pack file
layout with randomized membership (ADR-0005), manifest entry schema (path,
mode, size, chunk hashes, mtime, NFC-normalized name), last-agreed manifest
pointer, format version header. No code. The spec is the deliverable;
everything else implements it. Cross-check against restic's design doc and
the BEP spec cited in `research/landscape.md`.

Acceptance: an engineer could implement a compatible store in another
language from this document alone.

## Comments

2026-08-24: Done. `docs/store-format.md` (v1) covers all ticketed areas:
BLAKE3-256 plaintext addressing with ciphertext-hash-named packs, Rabin CDC
(window 64 B, 512 KiB / 1 MiB / 8 MiB, degree-53 per-folder polynomial with
generation + irreducibility test), pack layout with STREAM encryption,
encrypted trailing footer, atomic temp+rename, randomized membership
(W=8 staging packs, 16 MiB target), deterministic binary manifest schema,
last-agreed pointer record, FERRY-prefixed version header, wire note for
T-008, and a prior-art cross-check section citing restic and BEP with
one-line deviations.

Decisions made beyond the ticket text:

- ChaCha20-Poly1305 chosen over AES-256-GCM (software speed on AES-less
  endpoints; age lineage). Nonce = 8 zero bytes || u32 BE ((counter << 1) |
  last_flag); per-pack HKDF subkeys eliminate cross-pack nonce-reuse risk.
- Whole-pack continuous-stream encryption instead of restic's per-blob
  IV||CT||MAC, so individual chunk lengths are hidden outright, not just
  de-correlated by membership shuffling.
- The chunker polynomial is NOT serialized into the plaintext config-head;
  it ships as an encrypted blob (kind 0x04) because a plaintext polynomial
  plus any chunk-size oracle enables known-file confirmation. Config-head
  holds only public ids and wrapped keys.
- Manifests carry a parent pointer (lineage), but authority stays with the
  per-peer last-agreed record (ADR-0004).
- No BEP version vectors / sequence numbers / per-file block-size ladder:
  three-way reconciliation replaces them.
- Symlink targets must be valid NFC UTF-8; restic's raw-bytes escape hatch is
  dropped for v0.
- Index containers are encrypted under FMK and rebuildable from pack footers.

Cross-checked against restic design references (restic.readthedocs.io
100_references.html, the current home of the old design.html content) and
the Syncthing BEP v1 spec; both fetched 2026-08-24 and cited in the doc.
