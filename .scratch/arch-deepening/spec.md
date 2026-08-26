# SPEC: Architecture Deepening & Ponytail Reductions

**Feature Slug**: `arch-deepening`  
**Status**: `closed`  
**Date**: 2026-08-26  

## Goal

Surface architectural friction, eliminate over-engineering and code duplication (~2,200 LOC reduction), and deepen shallow modules into high-leverage, testable interfaces across the Ferry workspace.

## Scope & Tickets

1. `01-ponytail-and-platform-helpers.md` — Low-hanging ponytail reductions, M0 prototype deletion, unified `timefmt` in `ferry-platform`, hex deduplication.
2. `02-deep-pin-manager.md` — Encapsulate `PinStore`, `HeldLedger`, `PathMatcher`, `hold_filter`, and `plan_release` in `ferry-pin` behind a deep `PinManager`.
3. `03-direct-pingate-applier.md` — Direct `PinGate` integration in `ferry-materialize::Applier`, eliminating `ferry-sync/src/applier.rs` and the TOCTOU window.
4. `04-noise-handshake-securesession.md` — Encapsulate Noise handshake, DH exchanges, transcript hashing, and authenticated frame sealing in `SecureSession`.
5. `05-unified-dashboard-server.md` — Create a single deep `DashboardServer` driven by `DashboardBackend` adapters (`DirectBackend` and `IpcBackend`).
6. `06-wire-dashboard-and-retire-clones.md` — Wire `ferry daemon --ui` and `ferry ui` to `DashboardServer` and delete 750 LOC duplicate UI code in `ferry-cli`.
