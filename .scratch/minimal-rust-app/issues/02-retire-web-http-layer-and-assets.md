# Issue 02: Retire Web HTTP Layer and Embedded Assets

Status: ready-for-agent
Depends on: .scratch/minimal-rust-app/issues/01-eliminate-redundant-disk-fallbacks.md
Blocks: .scratch/minimal-rust-app/issues/04-standalone-inprocess-tui-runner.md

## Problem
The Axum HTTP server, token authentication middleware, and embedded HTML/CSS/JS assets introduce unnecessary HTTP/TCP serialization and browser dependencies.

## Proposed Solution
- Remove or deprecate `DashboardServer` and `crates/ferry-daemon/assets/`.
- Replace `ferry ui` command with a direct alias/pointer to `ferry tui` or retire `--ui` flags.

## Acceptance Criteria
- No Axum HTTP server required for UI operation.
- Binary size reduced by removing embedded HTML/CSS/JS assets.
- Clean compilation without dead code warnings.
