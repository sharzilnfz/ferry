# Spec: Architecture hardening pass (arch-hardening)

Status: in-progress

## Goal

One consolidated pass over the Ferry workspace that turns shallow modules
into deep ones, closes real race conditions and panic paths, and removes
duplicated implementations that have already caused drift bugs. Everything
lands on branch `arch-hardening`. No behavior regressions; existing tests
plus scripts (`quickstart-e2e.sh`, `skeleton-e2e.sh`, `adversarial-fixture.sh`)
must stay green, and CI gates (`cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`) must pass at every
merge point.

## Non-goals

- No protocol or store-format changes (ADR-0001..0005 are settled; this pass
  does not re-litigate them).
- No new features.
- No tokio migration — the codebase is deliberately std-threaded except
  ferry-iroh; we keep that choice.

## Findings inventory (from the exploration fan-out)

Three read-only audits (architecture/deepening, concurrency/testability,
rust-robustness) produced the candidate list below. Tickets reference these.

### A. Deepening candidates

| # | Finding | Strength |
|---|---------|----------|
| A1 | Two parallel sync stacks: `ferry-cli/src/exchange.rs` M0 loop vs `ferry-sync` v1 engine | Strong |
| A2 | Duplicated materializer: `ferry-sync/src/materialize.rs` `InlineMaterializer` vs `ferry-materialize::Applier` (already caused double-patch commits, e.g. 9c440a3) | Strong |
| A3 | Three last-agreed record codecs (`ferry-sync/state.rs`, `ferry-proto/agreement.rs`, `ferry-sync-engine/agree.rs`) | Strong |
| A4 | Snapshot walk rules duplicated between `ferry-store/snapshot.rs` and `ferry-scan/walk.rs` (equality by oracle test, not construction) | Worth exploring |
| A5 | Ignore seam drops entry kind → stat-in-hot-path adapter with double evaluation | Worth exploring |
| A6 | CLI hand-assembles crypto/store internals (`folder.rs`, `FolderSession`) | Worth exploring |
| A7 | Two unrelated `PeerState` types exported by `ferry-sync` and `ferry-sync-engine` | Speculative |

### B. Concurrency / correctness candidates

| # | Finding | Strength |
|---|---------|----------|
| B1 | Poll-tick publication races sessions: adopt/mint clobbering across four mutexes; torn-tree manifests offered as truth (`ferry-sync/engine.rs`) | Strong |
| B2 | Session pinning enforced in the CLI driver only, absent from the v1 engine path; pid-only liveness (no start-time check ⇒ pins stick forever after pid reuse); non-atomic `PinStore::start` with fixed temp name | Strong |
| B3 | No coordination between scanner passes and tree mutation (applier renames under a live walk) | Strong |
| B4 | Quarantine destination allocation is probe-then-create; cross-process rename can silently destroy a loser copy (ADR-0004 violation risk) | Worth exploring |
| B5 | `Ctx` god-struct: eight locks with convention-only ordering, spin-sleeps under the session lock, unbounded thread-per-connection, shutdown join race | Worth exploring |
| B6 | Store: one global `Mutex<Inner>`, full `rebuild_index()` per delivered pack, `build_pack_map` loads every pack fully into RAM | Worth exploring |
| B7 | NFC folding: per-component stat/readdir amplification; silent lexicographic min() arbitration when duplicate spellings coexist | Speculative |

### C. Rust best-practices candidates

| # | Finding | Severity |
|---|---------|----------|
| C1 | Public chunker API panics on user-supplied polynomial (`expect` in `chunk`/`chunk_offsets`; poly comes from CLI flag) | High |
| C2 | Attacker-influenced allocation: `Vec::with_capacity(wire_u32)` in manifest parse | Medium |
| C3 | `tlen_pos - tlen` unsigned underflow panic on corrupt index trailer | Medium |
| C4 | Whole-file buffering in scan rehash and applier; O(n²) `Vec::contains` in `LocationTable` | Medium |
| C5 | Doc claims constant-time MAC compare; code uses `!=` (`ferry-crypto/pairing.rs`) | Medium |
| C6 | `u32 as libc::pid_t` cast flips sign in unix pin liveness probe | Low |
| C7 | ~12 `lock().unwrap()` sites poison-cascade in long-running relay/iroh processes | Low |
| C8 | No `[workspace.lints]`; dep versions duplicated instead of `[workspace.dependencies]` | Low |
| C9 | CLI imports raw crypto primitives (chacha20poly1305/hkdf/sha2) bypassing `ferry-crypto` | Low |

## Ticket plan

Tickets live in `issues/NN-*.md`. Dependency spine: 01–04 are independent;
05 → 06 → 07 → 14 form the ferry-sync spine; 09 precedes 11 and 13; 15 last.

| Ticket | Covers | Waves after |
|--------|--------|-------------|
| 01 | C8 workspace hygiene | 1 |
| 02 | C1, C2, C3 (+index set fixes) | 1 |
| 03 | C5, C9 crypto hygiene | 1 |
| 04 | C7 poison-tolerant locks | 1 |
| 05 | A2 delete InlineMaterializer | 2 |
| 06 | B2 pinning enforcement + liveness (+C6) | 3 |
| 07 | B1+B5 folder-pointer state machine, bounded accept, shutdown | 4 |
| 08 | B4 exclusive quarantine landing | 2 |
| 09 | C4 streaming chunk/apply IO | 3 |
| 10 | A3 single agreed-record codec | 4 |
| 11 | A4 shared scan admission gate | 4 |
| 12 | A5 ignore seam carries entry kind | 2 |
| 13 | B7 NFC fold cache + loud duplicate-spelling | 5 |
| 14 | A1(+A7,A6-lite) retire CLI M0 stack | 5 |
| 15 | B6 store contention relief | 5 |

Deliberately dropped (recorded so future reviews don't re-suggest casually):
B3 full quiesce-token design and A7-as-standalone — both fall out of 07/14;
iroh runtime bridging (speculative, no observed failure).

## Acceptance for the whole feature

- All tickets `Status: done` with their individual acceptance criteria met.
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings
  && cargo test --workspace` green on the branch.
- `scripts/quickstart-e2e.sh` converges byte-for-byte.
- No public behavior change visible in `docs/cli-json.md` schema.

## Addendum (second audit fan-out, post T-01)

A fresh read-only audit verified the inventory above and added four tickets:

| Ticket | Covers | Waves after |
|--------|--------|-------------|
| 16 | proto: unbounded advert/batch receive loops; fixed temp name + no fsync in ingest_pack | 2 |
| 17 | Windows: colon/prefix manifest components escape the root; exec-bit test red on CI | 2 |
| 18 | TOFU peer identity never persisted/enforced (daemon accepts any authenticated id) — depends on 07 | 6 |
| 19 | ScanEngine::subscribe unbounded channel leaks snapshots behind stalled consumers | 3 |

Also folded into existing tickets: symlink-target policy gap in
InlineMaterializer → added to 05's scope (Applier already enforces
classify_link). Audit explicitly cleared: proto frame codec, pairing parse
offsets, secret-file permissions, store crypto schedule, iroh transport.

Execution model from wave 2 onward: each wave runs as PARALLEL sub-agents,
one git worktree + branch per ticket off the current arch-hardening head;
branches merge back after the wave with full gates re-run per merge.
