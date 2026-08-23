# T-002: Chunking and blob store

Status: ready-for-agent
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
