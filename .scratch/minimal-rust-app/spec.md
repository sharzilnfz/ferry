# Specification: Pure Native Rust Architecture (No HTML / No HTTP Daemon Bloat)

## 1. Executive Summary

By eliminating the Web/HTML layer (`crates/ferry-daemon/src/ui`, embedded assets, Axum HTTP server, browser spawn), the entire Ferry UI footprint collapses into a single, unified, ultra-lightweight pure Rust architecture:
1. **`ferry-cli`**: Fast subcommands for automation, scripting, and shell integration.
2. **`ferry-tui`**: Full-featured interactive terminal dashboard (`ratatui`) with pairing sheets, secret scanner warnings, conflict inspectors, transfer gauges, and device fleet telemetry.
3. **`ferry-ipc` / `ferry-folder`**: Unified deep engine seam. When a daemon is running, the TUI connects via zero-copy IPC; when standalone, it runs directly in-process without spinning up TCP ports or HTTP runtimes.

---

## 2. Architectural Streamlining (The Deletion Test)

### 2.1 What Gets Deleted / Retired
- **Axum HTTP Server (`crates/ferry-daemon/src/ui/server.rs`)**: Eliminates HTTP routing, JSON serialization over TCP loopback, and inactivity timers.
- **Embedded Web Assets (`crates/ferry-daemon/assets/`)**: Eliminates `index.html`, `style.css`, `app.js` (~65KB embedded binary bloat).
- **Redundant Disk Fallbacks (`crates/ferry-daemon/src/ui/backend.rs`)**: Eliminates ~600 lines of duplicated disk scanners, identity loaders, and secret checkers.
- **Token Auth Middleware & Browser Spawners**: Eliminates OS browser subprocesses (`open`, `xdg-open`, `cmd.exe`).

### 2.2 Deep Module Seams
```
┌──────────────────────────────────────────────────────────┐
│                   Unified Rust Interfaces                │
│                                                          │
│   ┌────────────────────────┐   ┌──────────────────────┐  │
│   │    ferry-cli (Cmds)    │   │   ferry-tui (App)    │  │
│   └───────────┬────────────┘   └──────────┬───────────┘  │
│               │                           │              │
│               └─────────────┬─────────────┘              │
│                             │                            │
│                             ▼                            │
│   ┌──────────────────────────────────────────────────┐   │
│   │        Unified Engine & Folder Access Seam       │   │
│   │  (ferry-ipc client OR in-process ferry-folder)   │   │
│   └──────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────┘
```

---

## 3. Revised Ticket Plan
- `01-eliminate-redundant-disk-fallbacks.md`: Delete duplicate disk scanners in UI backend.
- `02-retire-web-http-layer-and-assets.md`: Remove Axum HTTP web server, assets, and browser spawners.
- `03-deepen-tui-with-parity-features.md`: Add Secret Scanning warnings & Pairing flow directly to Ratatui TUI.
- `04-standalone-inprocess-tui-runner.md`: Allow `ferry tui` to run standalone in-process when daemon is offline.
- `05-end-to-end-verification.md`: Complete test and lint pass across workspace.
