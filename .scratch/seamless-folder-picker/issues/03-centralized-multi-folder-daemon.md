# 03: Centralized Multi-Folder Device Daemon & Registry

**What to build:** A centralized device-level background daemon running at `$FERRY_HOME/daemon.sock` that coordinates multiple active `SyncEngine` instances via a persistent registry (`$FERRY_HOME/folders.toml`). Frontends can list registered folders, add new ones, and switch the active folder context dynamically.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] Folder registry persists configured sync roots and settings under `$FERRY_HOME/folders.toml`.
- [ ] Central IPC socket at `$FERRY_HOME/daemon.sock` serves multi-folder status and dynamic folder registration commands.
- [ ] Daemon spawns and supervises isolated `SyncEngine` instances concurrently without cross-folder state interference.
- [ ] Frontends and `UiBackend` adapters (`InProcessAdapter`, `DaemonIpcAdapter`) implement `list_folders()` and `register_folder()`.
- [ ] Integration tests verify multi-folder synchronization under one device daemon process.
