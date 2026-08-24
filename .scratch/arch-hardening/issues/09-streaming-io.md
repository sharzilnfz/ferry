# T-09: Streaming chunk/read IO — stop buffering whole files in scan and apply

Status: ready-for-agent
Depends on: T-02 (chunker API)

Two whole-file buffers defeat 1 MiB CDC chunks:
1. Scan rehash slurps entire files: `std::fs::read` then chunk
(crates/ferry-scan/src/walk.rs:365-372). GB-scale assets spike RSS.
2. Applier holds EVERY chunk of a file in RAM before writing
(crates/ferry-materialize/src/apply.rs:757-776 write_file_atomically builds
`Vec<Vec<u8>>` of all chunks) — 2x file size peak plus temp copy.

Fix:
1. Add a streaming API to ferry-store chunker built on the existing
incremental `push()` (chunker.rs:239): e.g. feed(&[u8]) -> iterator of
completed chunk boundaries, plus finish(). Keep the old slice functions as
thin wrappers (now Result-returning per T-02).
2. Walk rehash streams the file through the chunker with a bounded read
buffer (e.g. 256 KiB–1 MiB), hashing chunks as they complete.
3. write_file_atomically streams chunks sequentially to the temp file,
verifying each chunk hash on the fly, keeping only the current chunk
resident; final whole-file verification semantics preserved (read back only
what the pipeline requires — do not weaken the re-hash guarantee).

Watch for behavior coupling: chunk boundaries must be byte-identical to the
buffered implementation (same poly, same windowing) — add a differential
test comparing streaming vs slice outputs on random inputs including sizes
around min/avg/max boundaries.

Acceptance: differential chunker test green; a large-file test (e.g. >32 MiB)
shows peak allocation stays bounded (assert via tracking allocator or simply
document + eyeball RSS in CI logs); all scan/applier suites green.
