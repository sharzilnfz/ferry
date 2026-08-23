# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Stack

Delegated: undecided. This repo currently holds planning documents only
(Rust core per `SPEC.md`; no frontend scaffold exists). Any future web
surface (landing page, docs, dashboard) must choose its stack when first
built; do not assume one before then.

## Users

Primary users are professional software developers working across two or
more machines (commonly desktop + laptop, often mixed macOS/Linux/Windows),
sometimes plus a WSL guest, a remote server, or a cloud dev box. Their job:
have their entire project directory — including everything git refuses to
carry (`node_modules`, `.env`, caches, datasets) — identical everywhere,
without manual sync rituals.

A growing second audience runs AI coding agents on always-on machines while
reviewing from laptops. They need agent state (`~/.claude`, `.opencode`,
`CLAUDE.md`) to follow them between machines, and they need safety when an
agent writes at machine speed while a human reads elsewhere. See
`research/use-cases.md` archetypes 1–10 for the full, cited breakdown.

## Product Purpose

Ferry (working name) is end-to-end encrypted, peer-to-peer file sync for
developer project directories. It exists because git carries tracked source
only, cloud drives corrupt developer directories, and expert tools
(Syncthing, Mutagen, Unison) demand expertise most developers don't have.
Success means: a developer installs, pairs two machines, and has identical
projects everywhere in under five minutes without reading docs; killing any
process at any moment loses no acknowledged data.

## Positioning

Sync stores, not trees: content-addressed manifests make delta detection a
cheap metadata diff, dedup falls out of hashing, and hydration pulls from
any peer. Combined with E2E encryption by default, blind relays, conflict
quarantine (never merge), and agent-aware session pinning, this is the
combination no competitor offers: Syncthing lacks dev semantics, Mutagen
lacks discovery/pairing, Bowline is closed-source, cloud drives lack E2E
and corrupt repos. Free OSS with an optional self-hostable relay.

## Operating Context

- Terminal-first product; CLI commands (`ferry init | pair | share |
  status | conflicts`) run inside a developer's shell, alongside git.
- Runs persistently as a daemon on macOS, Linux, Windows (incl. WSL).
- Works over LAN and the open internet via QUIC hole punching with relay
  fallback; peers addressed by device key, never IP.
- Coexists with git: Ferry carries untracked/uncommitted state; git keeps
  history. Ignore rules follow `.gitignore` conventions.

## Capabilities and Constraints

- Full-file bidirectional sync of declared folders; content-defined chunking;
  conflict quarantine files + structured report; never auto-merges contents.
- E2E encrypted (per-folder keys, device pairing, age-style envelopes);
  relays see ciphertext only. No accounts; losing all paired devices loses
  data (documented loudly).
- v0 non-goals: GUI, mobile, hosted services required for function, team
  permissions, version history beyond last-agreed state. A future web
  surface would be documentation/landing/dashboard work, separate from v0.
- Terminology per `CONTEXT.md` glossary: store, blob, manifest, tree,
  materialize/hydrate, folder, pairing, relay, conflict file, session
  pinning. Use these terms exactly.

## Evidence on Hand

- `research/use-cases.md` and `research/landscape.md`: cited competitive and
  user-pain research (Syncthing/Mutagen/Bowline/Tether comparisons, GitGuardian
  secret-sprawl numbers, WSL performance benchmarks).
- `SPEC.md` milestones M0–M4 with acceptance criteria; ADRs 0001–0005.
- No testimonials, customer logos, screenshots, or benchmarks exist yet.
  Future design work must not fabricate any of these; benchmark numbers
  arrive from T-004/T-002 acceptance gates.

## Product Principles

1. Zero silent data loss, ever — conflicts quarantine loudly; trust is the
   product.
2. Five minutes from install to synced machines, without documentation.
3. Encrypted by default, zero-knowledge infrastructure; convenience never
   trades away ciphertext blindness.
4. Developer-directory literacy: respects `node_modules` scale, `.gitignore`
   conventions, secrets hygiene, and agent workflows out of the box.
5. Local-first: fully functional without any vendor service.

## Accessibility & Inclusion

Cross-platform parity is a product requirement: features must behave
equivalently on macOS, Linux (incl. WSL), and Windows, and CLI output must
be machine-parseable (`--json`). No product-specific visual accessibility
standard has been established yet.
