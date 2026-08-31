# 01: Baseline harness and crate-graph snapshot

Status: ready-for-agent
Depends on: None
Blocks: 02, 03, 04, 06, 07

**What to build:** A rerunnable proof harness that establishes the current structural baseline so every later subtraction is verifiable as deletion not drift. From the operator and maintainer perspective this ticket makes the existing sync, pairing, and store behavior observable before any deletion lands.

**Blocked by:** None (can start immediately)

**Status:** ready-for-agent

- [ ] Harness asserts shipped behavior stays green via the highest seams: convergence through the convergence engine, pairing through the pairing ritual, and status through the backend contract (prior art: `ferry-sync-engine` matrix tests, `ferry-iroh` transport tests, `ferry-ipc` contract tests)
- [ ] Harness captures counts for the symbols that later tickets delete (global routing table, ferry-pin facade, backend triplication, cipher duplication, hex and state duplications, hand-rolled helpers) via text search and shows them as non-zero before and zero after each later ticket
- [ ] Harness captures crate graph shape (workspace crate count and external package count) and external behavior is unchanged after this ticket alone
- [ ] `cargo test` for the convergence, transport, daemon backend, and IPC seams passes on the baseline

## Comments

Tracer-bullet but prefactoring: no product behavior changes. All later tickets block on this because they assert deletion against its snapshot. Follows `prove-it-works` and `guard-the-context-window` via a single harness rather than per-ticket ad-hoc counts.
