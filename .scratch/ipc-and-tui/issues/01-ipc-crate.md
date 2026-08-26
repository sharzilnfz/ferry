# 01: ferry-ipc crate and wire protocol

**What to build:** A lightweight local IPC crate providing typed message framing over Unix domain sockets and Windows named pipes. Enables the sync daemon and local clients to exchange structured state messages and commands with zero external HTTP dependencies.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

## Implementation Notes for Agent
- Use `codebase-memory-mcp` to index project `ferry-sync` and inspect existing protocol definitions in `crates/ferry-proto`.
- Define `DaemonMessage` (server push) and `ClientCommand` (client requests) serialized as newline-delimited JSON.
- Provide async client and server connection helpers wrapping `tokio::net::UnixStream` / `UnixListener` on Unix, and named pipes on Windows.
- Provide in-memory duplex transport support for testing.

## Acceptance Criteria
- [ ] New crate `crates/ferry-ipc` compiles within the workspace.
- [ ] Serialization and deserialization unit tests pass for all `DaemonMessage` and `ClientCommand` variants.
- [ ] In-memory duplex transport tests verify clean framing, message ordering, and reconnection behavior.
- [ ] Platform socket path helper resolves to `~/.ferry/daemon.sock` or `<store>/.ferry/daemon.sock` on Unix and a named pipe on Windows.
