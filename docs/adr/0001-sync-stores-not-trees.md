# ADR-0001: Sync stores and manifests, not trees

Status: proposed (2026-08-23)

## Context

Every machine needs its working tree on disk for editors, build tools, and
agents. The naive design ships tree deltas directly between machines, like
Dropbox or Syncthing do. That couples sync correctness to filesystem quirks
and makes delta detection expensive (you must read file bytes to know what
changed).

## Decision

Each machine keeps a content-addressed store: CDC-chunked blobs addressed by
hash, plus manifests describing directory snapshots. The network layer
exchanges manifests first, then only missing blobs. Each machine materializes
its own tree from local store contents. Trees are never synced; they are
projections.

## Consequences

- Delta detection is a manifest diff: cheap, no byte reads.
- Dedup across files, versions, and machines falls out of content addressing.
- Hydration on a fresh machine can pull blobs from any peer that has them,
  in parallel, resumable.
- Cost: every write must be scanned and chunked into the store before it can
  sync. Scan performance becomes a first-class engineering problem (watchers,
  size/mtime short-circuits, background hashing).
- The store format is the compatibility contract between versions. It must be
  versioned from day one.
