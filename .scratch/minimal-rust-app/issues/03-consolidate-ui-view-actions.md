# Issue 03: Consolidate UI View Actions & Modals

Status: ready-for-agent
Depends on: .scratch/minimal-rust-app/issues/01-eliminate-redundant-disk-fallbacks.md
Blocks: .scratch/minimal-rust-app/issues/04-self-contained-rust-ui-app.md

## Problem
Frontend actions (Pair offer, Secret Scan Warning, Pin hold/release) across Web UI and TUI use different error formats and payload shapes, leading to fragmented view logic.

## Proposed Solution
- Standardize all action responses to the unified schema in `docs/cli-json.md`.
- Unify the Pair Modal and Secret Scanner warning views in `crates/ferry-daemon/assets/` to handle batch actions and clean token copying.

## Acceptance Criteria
- All action endpoints adhere to the canonical error schema (`{ code, error, hint }`).
- Modals smoothly handle transition between Secret Warning and Force Share (`i_know: true`).
