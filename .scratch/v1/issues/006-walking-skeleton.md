# T-006: Walking skeleton (M0)

Status: done
Depends on: T-002, T-003

Two processes on one machine sync a directory through the store over plain
localhost TCP. Deliberately throwaway transport and no encryption; its job is
to prove store → manifest diff → transfer → materialize end-to-end, and to
force every interface seam into existence early. Replaced by T-008/T-009.

Acceptance: script starts both daemons, touches 50 random files including an
append-heavy log file, asserts convergence within N seconds, tears down.
Runs in CI on macOS/Linux.

## Comments

Delivered in `crates/ferry-sync` (engine lib + `ferry-sync` daemon binary)
plus `scripts/skeleton-e2e.sh`. 111 workspace tests green; clippy
`--all-targets` clean; fmt clean.

### Seams the skeleton forced into existence (M0's deliverable)

1. **Transport** (`transport.rs`): `Transport`/`Listener`/`Connection`
   byte-frame pipes. `TcpTransport` = blocking localhost TCP, 4-byte LE
   length-prefixed frames, 512 MiB frame guard. T-009 swaps implementations;
   engine logic never sees sockets.
2. **Protocol messages** (`proto.rs`, internal/throwaway): HELLO, OFFER,
   REQ_META, REQ_DATA, ITEM, ITEMS_DONE, AGREED, ERROR. Encodings reuse spec
   primitives (`ferry_store::format`) and manifests move byte-for-byte as
   stored. Encryption OFF — T-008 wraps AEAD around payloads without
   touching flow control.
3. **Materializer** (`materialize.rs`): `Materializer` trait + `BlobSource`
   abstraction over "where blobs come from". Inline applier is ugly-but-
   correct: temp-file + atomic rename in-destination, parents-first creation,
   children-first deletion, exec bit, exact file/dir mtimes, symlink targets
   + link mtimes via `utimensat(AT_SYMLINK_NOFOLLOW)` (cfg unix). T-005's
   real applier implements the same trait against the same inputs.
4. **Agreement bookkeeping** (`state.rs`): spec-shaped last-agreed record
   (peer device id, manifest id, timestamp, flags) persisted per peer under
   `<store>/.ferry/sync/<tag>/`, plus the agreed manifest bytes so restarts
   recover the baseline root offline. T-010 consumes this as three-way base
   state.
5. **Donor selection** (`engine::select_donor`): deterministic direction
   choice both peers compute identically. Steady-state rule is CLOCK-FREE:
   whoever diverged from their last-agreed baseline sends. Fallbacks:
   non-empty beats empty on fresh bootstrap; manifest lineage only when both
   sides diverged simultaneously.
6. **Crypto insertion points**: store cipher stays v0 pass-through, FMK is
   zeros, frames plaintext — all isolated so T-007/T-008 add key material.

### Protocol decisions

- Sessions are pull-based: after HELLO×2 + OFFER×2, the puller walks the
  donor's tree level-by-level (REQ_META → ITEM* → ITEMS_DONE), diffs locally,
  then pulls data (REQ_DATA → ITEM* → ITEMS_DONE), materializes durably,
  THEN sends AGREED. Donor records agreement only on AGREED matching its own
  offer id. Any side may abort with ERROR; agreement state is untouched on
  failure and the next poll retries.
- The connector role drives sessions; the listener serves them and is
  discovered via the connector's opportunistic dials (every ~1s), which also
  carries listener-side edits back. Single dialer keeps M0 deadlock-free.
- Polling at 200 ms (ticket spec) with per-daemon session mutex; real
  watching is T-004's.

### Pack-vs-blob transfer choice

Data chunks are requested BY CHUNK ID and served as WHOLE PACKS named by
ciphertext hash whenever a needed chunk lives there (unmapped fallback:
individual blobs). Rationale: `docs/store-format.md`'s wire note fixes packs
as the transfer unit because `BLAKE3(ciphertext) == name` gives end-to-end
integrity for free — verified on receipt before anything touches store state
(a corrupted byte fails the name check, the session aborts with NO agreement
recorded, and retry converges; see tests/integrity.rs). Meta blobs (tree
nodes, manifests) move individually because the puller needs tree nodes
BEFORE it can compute what it wants. Received packs are ingested via atomic
rename into `packs/` + index rebuild — an M0 shortcut T-002/T-008 replace
with incremental appends.

### Known M0 limitations (documented, owned by later tickets)

- Simultaneous same-tick edits: resolved deterministically but may LOSE the
  loser's changes — T-010 owns conflicts/quarantine.
- Deleting every file in the tree does not propagate while the bootstrap
  empty-vs-nonempty guard exists (prevents a fresh empty device from wiping
  a populated peer).
- No ignore rules (T-011), no watching (T-004), no cross-machine transport/
  pairing/crypto (T-007/T-008/T-009).

### Timings (macOS arm64, debug build)

- Script: 5 consecutive runs, exit 0 each: convergence ≤1–2 s after the
  writer quiesces (N=30 budget); teardown leaves zero stray processes.
- In-process suite: convergence test (50 files + 250-line append-heavy log)
  2.6–3.2 s total including ~1.3 s of deliberate writer sleeps; bootstrap
  hydration of 27 files ≈0.7 s; corrupt-transfer-retry ≈0.6 s.

