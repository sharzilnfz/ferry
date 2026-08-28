# Issue 03: Deepen TUI with Parity Features (Pairing & Secret Scan Warnings)

Status: ready-for-agent
Depends on: .scratch/minimal-rust-app/issues/01-eliminate-redundant-disk-fallbacks.md
Blocks: .scratch/minimal-rust-app/issues/04-standalone-inprocess-tui-runner.md

## Problem
`ferry-tui` lacks the pairing flow and secret-scanning warning popups present in the web UI.

## Proposed Solution
- Add a Pair/Share modal sheet widget to `ferry-tui` (`crates/ferry-tui/src/ui.rs`).
- Display secret scan risk warnings in the TUI when sharing a folder with potential `.env` / key leaks.

## Acceptance Criteria
- Users can trigger and complete folder pairing directly inside the TUI.
- Secret scan warnings render clearly in terminal modals with override options.
