# Issue 04: Build Self-Contained Minimal Native Rust UI App

Status: ready-for-agent
Depends on: .scratch/minimal-rust-app/issues/02-unify-event-dispatch-and-kill-polling.md, .scratch/minimal-rust-app/issues/03-consolidate-ui-view-actions.md
Blocks: .scratch/minimal-rust-app/issues/05-end-to-end-verification.md

## Problem
Launching `ferry ui` currently spawns an external web browser and starts a local TCP server, introducing multi-process overhead, token security ceremony, and browser resource footprints.

## Proposed Solution
- Provide a lightweight, embedded Rust UI entry point that renders the exact same glassmorphism UI directly in a native desktop window (e.g. via embedded webview or direct protocol handler).
- Intercept UI API calls in-process over a direct message channel or local socket, bypassing TCP/HTTP and token auth when running natively.

## Acceptance Criteria
- Zero browser process spawn needed.
- Identical visual styling (Glassmorphism, beacons, telemetry strip, modals, themes).
- Immediate startup time (<50ms) and minimal memory footprint.
