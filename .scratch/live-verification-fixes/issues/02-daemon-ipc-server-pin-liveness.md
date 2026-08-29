# 02: Daemon IPC Server Binding & Long-Lived Pin Ownership

Status: done
Depends on: None
Blocks: 03-cli-pin-hours-and-ignore-folder.md, 09-e2e-live-process-and-browser-verification.md

**What to build:**
The background daemon must automatically bind and maintain an IPC server endpoint (domain socket or named pipe) for each watched folder. When a user runs `ferry pin start` from the CLI, the command communicates with the running daemon over IPC, transferring ownership of the pinned session to the daemon's active process ID. This ensures the pin remains alive and holding after the CLI command terminates, rather than prematurely degrading to stale.

**Blocked by:** None (can start immediately)

### Acceptance Criteria

- [x] Starting the background daemon automatically binds an IPC server endpoint scoped to the watched folder's state directory.
- [x] Running `ferry pin start` dispatches a pin command over IPC to the active daemon, which accepts and records the pin session under its own process identifier.
- [x] Running `ferry pin status` immediately after `ferry pin start` reports the pin as active and holding, with a live process ownership check.
- [x] If no daemon is running, `ferry pin start` displays a helpful error informing the user that background session protection requires an active daemon.
- [x] IPC connections and message exchanges gracefully handle connection timeouts, unexpected disconnections, and process shutdowns without hanging or leaving orphaned socket files.
- [x] Integration tests verify that pinned sessions established via CLI remain active and continue holding remote modifications across multiple subsequent CLI queries.

