# Performance findings — core (ferry-store, ferry-scan, ferry-sync-engine)

Audit scope: `crates/ferry-store/`, `crates/ferry-scan/`, `crates/ferry-sync-engine/`.
Method: full read of the scan→manifest→pack pipeline; cross-checked against the
one existing benchmark fixture (`benchmarks/scan.md`, produced by
`crates/ferry-scan/src/bin/bench-gate.rs`). No store-side or sync-engine
benchmark exists; all claims there are marked Suspected with the experiment
that would confirm them.

What did NOT survive reading the code (checked and cleared):

- **Re-hashing unchanged files**: the size/mtime/exec short-circuit is real and
  proven at scale — the bench gate measures a zero-change pass hashing 0 bytes
  (`bytes_chunked == 0`, benchmarks/scan.md:37-39, test at walk.rs:806-834).
  No finding here.
- **Chunks hashed twice on apply paths**: chunk ids are computed once at scan
  time and reused from manifests; `execute` re-hashes only for loser
  pre-verification, which is a correctness guard, not waste.
- **diff_roots**: merge-walks sorted entries with subtree pruning by id —
  already optimal in shape (diff.rs:242-277).

---

## Findings

### CORE-1
- Location: crates/ferry-store/src/index.rs:503-509 (callers: store.rs:707 `put_blob`, store.rs:767 `get`)
- Class: quadratic
- Measured: `LocationTable.candidates()` linear-scans the entire `entries` Vec per call. `put_blob` calls it once per blob put (store.rs:707) and `get` once per blob read (store.rs:767). A 100k-file initial scan does ≥100k puts against a table that grows to 100k+ rows → ~5×10⁹ `(kind,id)` comparisons. The Vec is in push order, not sorted, so binary search isn't available without restructure. Note the file's own comment (index.rs:477-479) fixed exactly this pattern for `merge` with a HashSet mirror but left `candidates` linear.
- Suspected vs measured: confirmed by code reading; no benchmark exercises put throughput. Confirm by timing `Store::put_data` over 100k chunks and observing superlinear wall time.
- Suggested fix: add a `(kind, id) -> Vec<IndexEntry>` HashMap mirror alongside `seen` (same pattern as the existing dedup mirror); keep the Vec only for `iter_sorted`. Pure in-memory change. **No wire-format or store-layout change — safe for v0.**

### CORE-2
- Location: crates/ferry-store/src/pack.rs:703 + pack.rs:623-625, via store.rs:782-793 (`Store::get`)
- Class: rehash (+ sequential-io)
- Measured: every single blob read reads the ENTIRE pack file into memory (`std::fs::read`) and BLAKE3-hashes the whole file twice effectively — name verification hashes all bytes (pack.rs:703), then the blob plaintext again after decrypt (pack.rs:846). Reading N blobs from one pack costs N × (full-pack read + full-pack hash). `diff_roots` (one tree node per get), `manifest_chunk_refs` (reconcile.rs:152-170), and `execute`'s FromStore quarantine path (execute.rs:379-391, one get per chunk) all sit on this path.
- Suspected vs measured: confirmed structurally by code; magnitude unmeasured (no read-path benchmark). Confirm by timing 100 sequential `store.get` calls against a 16 MiB pack.
- Suggested fix: cache an open pack handle (fd/mmap + verified flag) keyed by PackId so the name hash runs once per pack per session, not once per blob. Read-only change. **No format change — safe for v0.** (The per-blob verify-after-decrypt hash of the *blob* itself must stay; only the whole-file name hash is redundant repetition.)

### CORE-3
- Location: crates/ferry-store/src/chunker.rs:246-267, called per file from crates/ferry-scan/src/walk.rs:445 (and crates/ferry-store/src/snapshot.rs:524 via `chunk()`)
- Class: alloc-in-loop / other (per-file setup cost)
- Measured: `Chunker::new` runs `is_irreducible` (53 GF mulmods) + `gf_pow_x(504)` (~504 reduction steps) + builds a 256-entry out_table — for EVERY file streamed. On the bench fixture (files ≪ MIN_SIZE → one chunk each) this setup work is comparable to the actual chunking work per file. The benchmark report already names "chunker table warm-up per folder" as the next lever (benchmarks/scan.md:79) and identifies the scalar byte-at-a-time loop as the known hotspot (benchmarks/scan.md:74-75).
- Suspected vs measured: warm-up cost confirmed by code; its share of the 25.7 s initial scan is not isolated. Confirm by hoisting the table and re-running bench-gate.
- Suggested fix: cache the derived `(slide_out, out_table)` per ValidatedPoly (folder-lifetime memo, e.g. inside `StoreHandle` next to `poly`) instead of rebuilding per Chunker instance. Also delete the per-file `vec![0u8; 256 KiB]` read buffer (walk.rs:448) by reusing a scratch buffer across files in one pass. **No format change — safe for v0.** (Changing WINDOW/poly parameters would be format-frozen; not proposed.)

### CORE-4
- Location: crates/ferry-scan/src/walk.rs:382 + walk.rs:518-520 (`find_entry`)
- Class: quadratic
- Measured: `rebuild_dir` resolves each file's previous entry via `find_entry`, a linear `Vec::find` over the old directory's entries — O(entries²) name comparisons per rebuilt directory. Rebuilding one 1000-file dir costs ~500k string compares even when every file short-circuits. Every dirty-dir rebuild pays this; the incremental gate passes because dirty dirs were small (163 dirs, mostly 100-file).
- Suspected vs measured: confirmed by code; unmeasured at 1000+-file directory scale. Confirm with a bench fixture of one 10k-file directory plus a single change.
- Suggested fix: build a `HashMap<&str, &TreeEntry>` from `old_node.entries` once per rebuild_dir, replacing both `find_entry` and the identical inline scan in `DirCache::child_entry` (state.rs:43-50). Delete `find_entry`. **No format change — safe for v0.**

### CORE-5
- Location: crates/ferry-store/src/store.rs:998-1009 (`next_index_number`), called per sealed pack via store.rs:836-849 ← store.rs:980 (`seal_to_disk`)
- Class: quadratic
- Measured: every sealed pack appends an INDEX record whose numeric name is chosen by `read_dir`-scanning ALL existing index records to find the max. After N packs, sealing pack N+1 scans N directory entries → O(N²) readdir+parse-string work across a long-lived store (a 100k-file folder at 16 MiB packs ≈ hundreds of packs per full sync, repeated every burst).
- Suspected vs measured: confirmed by code; unmeasured. Confirm by setting `set_seal_target(small)` and timing flush #k as k grows.
- Suggested fix: cache `next_index_number`'s result in the `Store` (it is monotonic within the process and guarded by `index_seq`; on open, seed once from disk). Deletes the rescan entirely. **No layout change — safe for v0.**

### CORE-6
- Location: crates/ferry-scan/src/engine.rs:311 (run_full) / engine.rs:365-395 (run_incremental)
- Class: lock-contention
- Measured: the engine's `core` Mutex is held across the ENTIRE `Walker::run` — including all filesystem I/O, chunking, hashing, and pack writes (25 s+ for an initial full scan). During that window the poller thread blocks on `core` for its liveness check (engine.rs:706) and stat sweeps (engine.rs:729-731), `last_pass()` readers block, and concurrent `scan_once` callers queue behind the pass.
- Suspected vs measured: contention confirmed by control flow; impact today is bounded because there is one worker thread. Becomes real the moment hashing parallelizes (the documented lever, benchmarks/scan.md:77-78).
- Suggested fix: split the pass-relevant fields (handle, cache, prev ids) into their own mutex, or take snapshots of the small scalars before the walk and re-merge after. Careful, medium-size refactor. **No format change — safe for v0.**

### CORE-7
- Location: crates/ferry-store/src/store.rs:705-712 (`put_blob` dedup probe)
- Class: sequential-io
- Measured: every `put_blob` stats each candidate pack (`self.pack_path(&e.pack).is_file()`, store.rs:708) under the inner lock to decide dedup — a syscall per known location per put whenever content repeats (re-puts of unchanged tree nodes/manifests hit this every pass).
- Suspected vs measured: confirmed by code; frequency depends on workload (new-content puts skip it since candidates are empty). Confirm by counting `stat` syscalls during a zero-change incremental pass that republishes tree nodes.
- Suggested fix: negative-cache dangling pack ids, or accept the candidate list without existence-probing and fall back at stage time. Small. **No format change — safe for v0.**

### CORE-8
- Location: crates/ferry-sync-engine/src/reconcile.rs:152-170 + reconcile.rs:199-200, 421-422
- Class: other (repeated full-tree traversal through the expensive read path)
- Measured: one reconcile loads both trees at least three times: two `diff_roots` walks plus `manifest_chunk_refs(store, remote)` and `manifest_chunk_refs(store, local)`. Each tree-node load is a full `Store::get` (see CORE-2), so tree nodes are re-read and packs re-hashed multiple times per cycle. The local_refs walk exists solely to compute the `fetch` list ("not referenced anywhere locally").
- Suspected vs measured: confirmed by code; unmeasured (no reconcile benchmark). Confirm by instrumenting `store.get(BlobKind::TreeNode, ..)` counts during one reconcile of a 100k-entry manifest.
- Suggested fix: have `diff_roots` optionally emit loaded nodes (or collect refs in the same recursion), collapsing three traversals into one per side; combine with CORE-2's handle caching which shrinks the remaining cost. Medium. **No wire-format change — safe for v0.**

### CORE-9
- Location: crates/ferry-scan/src/walk.rs:275
- Class: alloc-in-loop
- Measured: every rebuilt directory deep-clones its entire cached `TreeNode` (`self.cache.node(rel).map(|c| c.node.clone())` — all entries with chunk lists) just to look up prior entries while the mutably-borrowed cache stays alive across the rebuild. For a dirty root over 100k files this clones the whole root listing per pass.
- Suspected vs measured: confirmed by code; cheap in absolute terms per pass unless directories are huge. Would confirm via heap profiling of one whole-tree incremental pass.
- Suggested fix: take the old node out of the cache for the duration of the rebuild (`HashMap::remove` + re-insert at line 430) instead of cloning. Delete the clone. **No format change — safe for v0.**

### CORE-10
- Location: crates/ferry-sync-engine/src/reconcile.rs:480-488
- Class: quadratic
- Measured: ancestor-suppression checks each survivor path's prefixes against `removal_keys`, a plain `Vec<String>`, with `contains` — O(survivors × depth × removals). Bounded by conflict/deletion counts, which are typically tiny.
- Suspected vs measured: confirmed by code, but realistic n is small; flagged for completeness, not urgency. Same shape at reconcile.rs:498-539 (that loop uses BTreeMap lookups, fine).
- Suggested fix: make `removal_keys` a `HashSet<String>` (one-line). **Safe for v0.**

### CORE-11
- Location: crates/ferry-store/src/pack.rs:1035-1050, under lock at crates/ferry-store/src/store.rs:752-762
- Class: lock-contention
- Measured: every `get` first locks `inner` and linearly scans all staging-pool entries (`staged_bytes`) for read-your-writes; every `put` holds the same lock while memcpy-ing blob bytes into staging bodies (up to MAX_SIZE = 8 MiB per chunk, store.rs:714-727). Pools are ≤8 packs but a pack body can hold thousands of small chunks.
- Suspected vs measured: confirmed by code; contention is latent until multiple threads share a Store (the doc-comment concurrency model says any number may).
- Suggested fix: move the staged-bytes membership index out of the memcpy critical section, or use a RwLock. Low priority. **Safe for v0.**

---

## Ranked summary

| ID | Impact | Effort | Safe-for-v0-fix |
|----|--------|--------|-----------------|
| CORE-1 | high | M | yes |
| CORE-2 | high | L | yes |
| CORE-3 | high | S | yes |
| CORE-4 | med | S | yes |
| CORE-5 | med | S | yes |
| CORE-6 | med | M | yes |
| CORE-7 | low | S | yes |
| CORE-8 | med | M | yes |
| CORE-9 | low | S | yes |
| CORE-10 | low | S | yes |
| CORE-11 | low | M | yes |

No finding requires changing the wire format or the on-disk store layout;
nothing here touches v0-frozen constants (window size, polynomial degree,
chunk bounds, container framing).
