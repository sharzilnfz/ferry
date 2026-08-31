# 04: Transparent background daemon auto-spawning for CLI and UI commands

**What to build:** Whenever a user executes any Ferry command (such as share, join, web dashboard, or terminal TUI), Ferry verifies whether a background daemon process is already active. If no daemon is running, Ferry automatically spawns a detached background daemon process, waits for its IPC socket to become responsive, and proceeds with the requested command seamlessly without requiring a dedicated open terminal tab.

**Blocked by:** 03: Automatic local network mDNS and mesh peer discovery in daemon supervisor

**Status:** ready-for-agent

- [ ] Platform helper checks daemon process lock and IPC socket liveness before running CLI and UI commands
- [ ] If the daemon is not running, the background daemon is launched as a detached child process
- [ ] The command waits for socket readiness with a timeout before proceeding
- [ ] Daemon status and stop commands correctly query and terminate the auto-spawned process
- [ ] Automated integration tests verify that running commands in clean environments launches the daemon in the background and succeeds cleanly
