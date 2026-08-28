# Issue 04: Standalone In-Process TUI Runner

Status: ready-for-agent
Depends on: .scratch/minimal-rust-app/issues/02-retire-web-http-layer-and-assets.md, .scratch/minimal-rust-app/issues/03-deepen-tui-with-parity-features.md
Blocks: .scratch/minimal-rust-app/issues/05-end-to-end-verification.md

## Problem
Running `ferry tui` currently fails if `ferry daemon` is not already running on the IPC socket.

## Proposed Solution
- Support a fallback in `ferry_cli::commands::tui` to run an in-process read/scan loop when no daemon socket is found.

## Acceptance Criteria
- `ferry tui` launches instantly even without an active background daemon.
- Zero idle CPU usage during in-process execution.
