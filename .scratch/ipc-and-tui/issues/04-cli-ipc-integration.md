# 04: CLI IPC integration and offline fallback

**What to build:** Update standard CLI commands (`ferry status`, `ferry conflicts`, `ferry pin`) to query the daemon IPC socket first for instant in-memory responses, falling back cleanly to direct store and disk metadata reads when the daemon is stopped.

**Blocked by:** 02: Daemon IPC server and engine broadcast.

**Status:** done

## Implementation Notes for Agent
- Use `codebase-memory-mcp` to inspect `crates/ferry-cli/src/commands/status.rs`, `pin.rs`, and `conflicts.rs`.
- Add an IPC client query helper in `ferry-cli` that attempts to connect to the local socket with a short timeout (50ms).
- If the socket responds, construct CLI output (human and `--json`) from the returned `EngineSnapshot`.
- If the socket is missing or errors, execute the existing direct-disk scan and store inspection path.
- Add `ferry tui` command that connects to the IPC socket and starts `ferry-tui::TuiApp`.

## Acceptance Criteria
- [x] `ferry status` returns instant cached status from the running daemon without initiating a disk rescan.
- [x] `ferry status --json` output schema matches `docs/cli-json.md` exactly whether querying over IPC or running offline.
- [x] `ferry pin start` and `ferry pin release` dispatch commands over IPC to the active daemon.
- [x] Stopping the daemon and running `ferry status` falls back to direct disk reads without crashing.
