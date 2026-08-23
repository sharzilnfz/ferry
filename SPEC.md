# SPEC: Ferry v0 (working name)

Full-file, end-to-end encrypted, peer-to-peer sync of developer project
directories across macOS, Linux, and Windows, designed for humans working
beside AI coding agents.

Base material: `research/use-cases.md`, `research/landscape.md`, decisions in
`docs/adr/`, vocabulary in `CONTEXT.md`. Read all three before implementing
anything.

## Product thesis

Developers lose hours to the gap between "what git carries" and "what my
project actually is": uncommitted work, `node_modules`, `.env`, caches,
datasets, agent memory. Cloud drives destroy dev directories; expert tools
(Syncthing, Mutagen, Unison) demand expertise nobody has; commercial entrants
(Bowline) are closed. The wedge is a one-command, zero-account, E2E-encrypted
sync that treats a dev directory as a first-class citizen and an agent as a
normal (if fast) writer.

## Primary personas (from research)

1. **Solo multi-machine dev** (desktop + laptop, possibly mixed OS). Wants:
   open laptop, project is just there, including WIP.
2. **Agent wrangler**: runs long agent sessions on an always-on machine;
   reviews from a laptop; needs agent state (`~/.claude`, `.opencode`,
   CLAUDE.md) to follow them and needs no torn writes while agents run.
3. **Heavy-asset dev** (ML/game): gigabytes of binaries present on every
   machine without Perforce money or LFS pain.

v0 optimizes for personas 1 and 2. Persona 3 benefits automatically from CDC
chunking but gets no special features yet.

## Scope

### In scope for v0

1. **Store core** (Rust crate): CDC chunking (ADR-0005), hash-addressed
   blobs, pack files, manifests (tree snapshots), incremental scan driven by
   file watchers with size/mtime short-circuits and periodic audits.
   Store format versioned and documented from day one (ADR-0001).
2. **Materialization**: produce/update the tree from store contents using
   atomic temp-file renames (Syncthing-style), verified hashes on every
   received block.
3. **Sync engine**: manifest exchange, three-way reconciliation against the
   last-agreed manifest (Mutagen-style base state), conflict quarantine per
   ADR-0004, ignore rules (gitignore syntax, tuned defaults).
4. **Transport**: iroh-based QUIC P2P keyed by device public key, blind relay
   fallback, LAN discovery (ADR-0003). Resume-safe chunked transfer.
5. **Crypto**: per-folder keys, device pairing via short code / QR, age-style
   wrapped key envelopes, AEAD on every blob and manifest (ADR-0002).
6. **CLI-first UX**:
   - `ferry init` inside a project directory (or `ferry add <path>`)
   - `ferry pair` (device pairing)
   - `ferry share <folder>` / accept flow
   - status/list/conflicts commands
   - daemon mode with watch + continuous sync; single-shot mode too
7. **Cross-platform correctness guardrails** (from landscape research):
   case-conflict detection from day one, NFC name normalization, Windows long
   paths via `\\?\`, explicit symlink policy (default: sync as links where
   safe, refuse dangerous cases loudly), documented permission subset (exec
   bit only).

### Out of scope for v0

- GUI, mobile, web previews
- Hosted anything required for function (relay is self-hostable; community
  relays later)
- Team management, permissions, sharing to third parties (single-user,
  multi-device only)
- File versioning/history beyond last-agreed state (backup is a different
  product)
- VCS integration beyond respecting `.gitignore` conventions
- Selective per-file sync UI (folder-level rules only)

## Milestones (tracer-bullet order)

- **M0 — walking skeleton**: two processes on one machine sync a directory
  through the store over a local transport. Ugly, but end-to-end.
- **M1 — real store**: CDC chunking, packs, incremental scan, watchers,
  materialization with atomic renames. Benchmarked on a synthetic
  node_modules-scale fixture (100k files).
- **M2 — real network**: iroh transport, device identity, pairing, encrypted
  blob/manifest exchange, relay fallback against a self-hosted relay.
- **M3 — safety**: three-way reconciliation, conflict quarantine + report,
  case-conflict detection, cross-platform CI (macOS, Linux, Windows runners),
  crash-safety tests (kill -9 at random points; store never corrupts, worst
  case is redo work).
- **M4 — agentic edge**: agent-state presets (`.claude`, `.opencode`,
  CLAUDE.md, AGENTS.md folders), session pinning prototype, secret-scan
  warning when a folder's rules include `.env`.

Definition of done for v0: a stranger on each of the three OSes can go from
install to synced-between-two-machines in under five minutes without reading
docs, and killing any process at any moment loses no acknowledged data.

## Risks

- Scan throughput on huge directories dominates perceived speed. Benchmark
  from M1 onward, before polish.
- iroh API churn: isolate behind a transport trait.
- Conflict-file litter during heavy concurrent agent writes: measure in M4,
  tune session pinning if it's bad.
- Name collision ("Ferry" is a placeholder; check crates.io/homebrew before
  publishing anything).
