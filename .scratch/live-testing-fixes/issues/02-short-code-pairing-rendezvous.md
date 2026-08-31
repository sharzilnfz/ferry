# Issue 2: Persistent multi-process rendezvous and CONFIG_HEAD wrap for short-code pairing

Status: ready-for-agent
Feature: `live-testing-fixes`
Depends on: .scratch/live-testing-fixes/issues/01-held-manifest-pin-release.md
Blocks: .scratch/live-testing-fixes/issues/03-tui-pin-toggle-active-state.md

## Context
`ferry share` creates an ephemeral in-process pairing session (`shared_rendezvous()`) and exits immediately. Running `ferry join <CODE>` from another process or terminal fails with `code: "pairing-not-found"`. Additionally, `share` does not update the sharer's `CONFIG_HEAD` allow-list with the joiner's device public key, causing daemon handshakes to fail with deny-unknown.

## Target Files
- `crates/ferry-folder/src/pairing.rs`
- `crates/ferry-cli/src/commands/share.rs`
- `crates/ferry-cli/src/commands/join.rs`

## Requirements
1. Implement a persistent local rendezvous backing (e.g. filesystem-backed rendezvous in `$TMPDIR/ferry-rendezvous/` or `$FERRY_HOME/rendezvous/`) so cross-process `ferry share` and `ferry join` discover each other.
2. Complete the key wrap on the sharer side so the joiner's device public key is added to the sharer's `CONFIG_HEAD` allow-list.
3. Clean up rendezvous files once consumed or expired.
4. Add an automated test verifying `ferry share` in process 1 + `ferry join <CODE>` in process 2 pairs successfully and subsequent daemon sync converges without handshake authorization denials.
