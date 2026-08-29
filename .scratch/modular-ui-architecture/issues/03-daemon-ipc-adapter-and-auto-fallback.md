# 03: Daemon IPC Client Adapter (`DaemonIpcAdapter`) & `AutoBackend`

**What to build:** A remote IPC implementation of `UiBackend` that connects to the sync daemon over Unix domain sockets or Windows named pipes (`ferry-ipc`), plus a composite `AutoBackend` adapter that attempts IPC connection first and automatically falls back to `InProcessAdapter` when the daemon is offline.

**Blocked by:** 01 (Core `UiBackend` Trait), 02 (In-Process Engine Adapter)

**Status:** ready-for-human

- [x] `DaemonIpcAdapter` satisfies `UiBackend` by dispatching typed `ClientCommand` messages and parsing `DaemonMessage` responses over `IpcClient`.
- [x] `AutoBackend` automatically queries `DaemonIpcAdapter` when the IPC socket exists and responds, and seamlessly delegates to `InProcessAdapter` if the daemon connection fails or is offline.
- [x] Frontends using `AutoBackend` require no manual flags to switch between daemon mode and standalone mode.
- [x] Integration tests verify that starting and stopping a mock IPC daemon transitions `AutoBackend` between remote and in-process execution transparently.
