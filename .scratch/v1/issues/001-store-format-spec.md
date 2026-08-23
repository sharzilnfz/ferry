# T-001: Store format spec v1

Status: ready-for-agent
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
