# Issue 1: Persist un-adopted remote manifests into Store during daemon holds

Status: ready-for-agent
Feature: `live-testing-fixes`
Depends on: 
Blocks: .scratch/live-testing-fixes/issues/02-short-code-pairing-rendezvous.md

## Context
When an incoming sync exchange encounters paths matched by an active session pin on Device A, `ferry-sync::exchange::Exchange` holds the incoming changes and writes entries to `.ferry/held/<peer>.jsonl`. However, because `outcome.held > 0`, it skips `self.store.put_meta(BlobKind::Manifest, &man_bytes)`.

When the user runs `ferry pin release`, `PinManager::release_peer` tries to retrieve the remote manifest via `store.get(BlobKind::Manifest, &id)`, which fails with:
```text
error: held manifest <hash> is missing from the store (code=held-manifest-missing)
hint: the held change's manifest left the store; release cannot reconstruct it safely
```

## Target Files
- `crates/ferry-sync/src/exchange.rs`
- `crates/ferry-sync-engine/src/converge.rs`
- `crates/ferry-sync-engine/src/pin/manager.rs`
- `crates/ferry-cli/src/commands/pin.rs`

## Requirements
1. In `ferry-sync::exchange::Exchange::exchange_folder()`, ensure remote manifest bytes are stored via `self.store.put_meta(BlobKind::Manifest, &man_bytes)` whenever changes are held (`outcome.held > 0`).
2. In `ferry-sync-engine::converge::ConvergenceEngine` / `PinManager`, ensure held manifests can be safely loaded from `Store` and reconciled against the base manifest during `pin release`.
3. Ensure `PinManager::release` performs three-way reconciliation, creates `<file>.ferry-conflict.<device_short>-<timestamp>` for collisions, records to `.ferry/conflicts.jsonl`, deletes `.ferry/held/<peer>.jsonl`, and transitions pin state to ended.
4. Add an automated test verifying `ferry pin start` -> remote edit -> `ferry pin release` produces quarantine files and logs to `conflicts.jsonl` with exit code 0.
