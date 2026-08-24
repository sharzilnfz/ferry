# idea2: secure full-file sync for developers and their agents

[![ci](https://github.com/sharzilnfz/ferry/actions/workflows/ci.yml/badge.svg)](https://github.com/sharzilnfz/ferry/actions/workflows/ci.yml)


Working name: **Ferry** (placeholder; rename during grilling if a better name lands).

## One-line pitch

Your entire project directory, identical on every machine you touch, including
everything git refuses to carry: `node_modules`, `.env`, build caches, agent
state, datasets. End-to-end encrypted, peer-to-peer when possible, relayed when
not, fast enough that switching machines mid-task feels like nothing.

## Why this exists

Git syncs tracked source. Nothing good syncs everything else:

- Dropbox and Google Drive choke on symlinked directories and corrupt
  `node_modules`; they also read every byte of your code on their servers.
- Syncthing is close but general-purpose: conflict handling is crude, dev-heavy
  directories are painful, and nothing speaks to agent workflows.
- rsync/scp are manual and one-directional.
- AI coding agents made all of this worse. An agent can churn through thousands
  of files overnight on your desktop while you open the same project on a laptop
  across town. Today there is no safe story for that.

## Design stance (decided up front, challenge via grilling)

1. Sync **stores**, not trees. Every machine keeps a content-addressed object
   store (hash-addressed blobs + manifests). The network layer moves blobs and
   manifests; each machine materializes its own working tree locally. Hash
   equality makes delta detection free.
2. **End-to-end encrypted by default.** No plaintext on any relay or disk
   outside your devices. Device pairing via QR / short code.
3. **Peer-to-peer first** (QUIC with NAT traversal), self-hostable relay as
   fallback. No vendor cloud required; a hosted relay may exist later but is
   never load-bearing.
4. **Conflicts quarantine, never merge.** Concurrent edits produce explicit
   conflict files plus a report, like Syncthing but louder and more structured,
   because silent merges destroy trust and agents make concurrent writes common.
5. **Agentic workflows are a first-class target**, not an afterthought:
   selective sync of agent state directories, session pinning, ignore rules
   tuned for `node_modules`-scale directories, hydration of heavy deps from any
   peer that already has them.

## Relationship to idea1 (`../idea1`)

Idea1 builds a local content-addressed store to end small-file churn on one
machine. Idea2 borrows the *shape* of that store (blobs + manifests + local
materialization) but is an independent project: its hard problems are
networking, crypto, conflicts, and cross-platform file semantics, none of which
idea1 owns. If both projects mature, a shared store-format spec could be
extracted later. Until then they share nothing at the code level.

## Documents

- `research/use-cases.md` — archetypes, use cases, pain points (web-researched)
- `research/landscape.md` — competitors and techniques worth borrowing (web-researched)
- `CONTEXT.md` — glossary
- `docs/adr/` — architecture decision records
- `SPEC.md` — v0 specification
- `.scratch/v1/issues/` — tracer-bullet tickets with blocking edges
- `KICKOFF-PROMPT.md` — paste this into a fresh session opened in this directory
