# Issue 01: Eliminate Redundant Disk Fallbacks

Status: ready-for-agent
Depends on: none
Blocks: .scratch/minimal-rust-app/issues/02-retire-web-http-layer-and-assets.md, .scratch/minimal-rust-app/issues/03-deepen-tui-with-parity-features.md

## Problem
`crates/ferry-daemon/src/ui/backend.rs` contains duplicate disk fallback routines that re-implement `ferry-folder`, `ferry-pin`, and `ferry-ignore` logic.

## Proposed Solution
- Cut redundant scan/identity logic. Delegate directly to `ferry-folder` and `ferry-sync-engine`.

## Acceptance Criteria
- Zero redundant scanner code.
- Workspace unit tests pass cleanly.
