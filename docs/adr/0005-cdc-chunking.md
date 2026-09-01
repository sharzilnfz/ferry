# ADR-0005: Content-defined chunking, with chunk-size leakage mitigated

Status: accepted (2026-08-23)

## Context

Fixed chunks lose dedup efficiency after insertions; CDC (FastCDC-style)
keeps boundaries stable under edits and Google measured 1500 MB/s effective
diffing versus rsync's 50 MB/s. But a 2025 attack paper broke keyed-CDC
schemes in Borg, restic, bupstash and others via chunk-length fingerprinting.

## Decision

- CDC chunking (FastCDC or restic/chunker lineage), target range ~64 KiB to
  8 MiB, per-folder random polynomial.
- Encrypt at the pack level so individual chunk boundaries are not observable
  on the wire or on untrusted storage: blobs are grouped into encrypted pack
  files with randomized membership (restic 0.18's mitigation direction).
- Benchmark fixed-vs-CDC on real dev directories before locking constants;
  revisit if a provably secure KCDC construction becomes practical.

## Amendment (2026-08-30): FastCDC Parameters & Pack Pooling

The implementation locks standard FastCDC parameters (minimum chunk 64 KiB,
average/target chunk 1 MiB, maximum chunk 8 MiB) using gear-based rolling
hashing. Pack files are encrypted with ChaCha20-Poly1305 and indexed by byte
offset headers, achieving leak-resistant chunk deduplication without sacrificing
high-speed tree scanning and streaming hydration.

## Consequences

- Pack-level encryption complicates random-access reads slightly; acceptable.
- The store format spec must document the polynomial and packing rules so
  independent implementations stay compatible.
