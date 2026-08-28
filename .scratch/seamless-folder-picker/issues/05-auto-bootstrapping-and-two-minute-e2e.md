# 05: Auto-Bootstrapping Daemon & Two-Minute Quickstart Workflow

**What to build:** Automatic background daemon bootstrapping on `ferry` startup and streamlined `ferry share <folder>` and `ferry join <code> [dest]` CLI shortcuts. Delivers the complete end-to-end user experience and rewrites the manual testing guide to the 2-minute workflow.

**Blocked by:** 03: Centralized Multi-Folder Device Daemon & Registry, 04: Zero-File In-Band Network Pairing via Short Codes

**Status:** ready-for-agent

- [ ] Running `ferry` in an interactive shell checks for `$FERRY_HOME/daemon.sock`, auto-spawns the background daemon if needed, and launches the default frontend.
- [ ] `ferry share <folder>` and `ferry join <code> [dest]` CLI commands allow two-command synchronization from the terminal.
- [ ] Acceptance script `scripts/zero-config-e2e.sh` verifies zero-configuration setup across two separate `$FERRY_HOME` environments.
- [ ] `MANUAL_TESTING_GUIDE.md` and `README.md` updated to document the new 2-minute quickstart workflow.
