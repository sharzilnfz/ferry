# 06: Ephemeral on-demand Web UI (ferry ui)

**What to build:** An on-demand `ferry ui` command that launches a short-lived local web server on a random loopback port (`127.0.0.1:0`), prints a one-time access token, opens the browser automatically, proxies queries to the daemon IPC socket, and shuts down automatically after 10 minutes of inactivity.

**Blocked by:** 02: Daemon IPC server and engine broadcast.

**Status:** ready-for-agent

## Implementation Notes for Agent
- Use `codebase-memory-mcp` to inspect existing web assets in `crates/ferry-daemon/assets/` and `crates/ferry-daemon/src/ui/`.
- Move the Axum routing and static asset embedding from `ferry-daemon` into a dedicated submodule or command in `ferry-cli`.
- When `ferry ui` runs:
  - Bind to `127.0.0.1:0` (random available port).
  - Generate a secure 32-character random hex token.
  - Connect to the daemon IPC socket to serve `/api/status`, `/api/conflicts`, `/api/share`, `/api/pair/accept`.
  - Launch default browser using platform openers (`open` on macOS, `xdg-open` on Linux, `start` on Windows).
  - Include an activity timer: reset on every HTTP request; if no requests arrive within 10 minutes, shut down the server cleanly.

## Acceptance Criteria
- [ ] `ferry ui` starts Axum on a random loopback port and opens the browser.
- [ ] Requests without the valid one-time token are rejected with HTTP 403 Forbidden.
- [ ] Web dashboard endpoints (`/api/status`, `/api/conflicts`) fetch live data from the daemon over IPC.
- [ ] Server shuts down automatically after 10 minutes of inactivity or on `Ctrl+C`.
