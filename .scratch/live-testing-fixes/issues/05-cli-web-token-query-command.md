# Issue 5: CLI command / helper to retrieve active Web UI URL and token

Status: ready-for-agent
Feature: `live-testing-fixes`
Depends on: .scratch/live-testing-fixes/issues/04-tui-backend-disconnected-handling.md
Blocks: .scratch/live-testing-fixes/issues/06-compiler-warnings-and-dead-code-cleanup.md

## Context
When running `ferry ui --web` (especially in background mode, CI, or when the user detaches from the initial terminal), the one-time authentication token is only printed to stdout at server startup. If that output is lost or scrolled past, there is no way to retrieve the token without killing and restarting the Web UI server.

## Target Files
- `crates/ferry-daemon/src/ui/server.rs`
- `crates/ferry-daemon/src/ui/mod.rs`
- `crates/ferry-cli/src/commands/ui.rs`
- `crates/ferry-cli/src/cli.rs`

## Requirements
1. Record active Web UI session metadata (port, host, auth token, PID) to `.ferry/web_session.json` or `$FERRY_HOME/web_session.json` when the server starts, and clean it up upon shutdown.
2. Add a CLI subcommand or flag (e.g. `ferry ui token [folder]` or `ferry ui --token [folder]`) that reads the recorded session and prints the active Web URL with token:
   `http://127.0.0.1:<port>/?token=<token>`
3. If no active Web UI server is running, return a clear error `code: "no-active-web-ui"`.
4. Add CLI tests verifying session file creation, token retrieval command, and cleanup on exit.
