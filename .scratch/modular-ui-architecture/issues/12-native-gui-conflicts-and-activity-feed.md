# 12: Native GUI Conflict Resolution & Activity Feed

**What to build:** Build the real-time activity log stream and conflict quarantine inspection modal in `ferry-gui`, giving developers clear visibility into incoming file sync events and quarantined conflict copies.

**Blocked by:** 09 (Native GUI Crate Bootstrap), 03 (Daemon IPC Adapter)

**Status:** ready-for-agent

- [ ] Live activity stream displays chronological entries (scans, chunk transfers, pins, errors) fed directly from `UiBackend::subscribe_events()`.
- [ ] Conflicts modal lists all entries in `.ferry/conflicts.jsonl` with timestamps, winning/losing devices, and quarantined file paths (`path.ferry-conflict.<device>-<ts>`).
- [ ] Activity log includes a "Clear" button and auto-scroll capabilities.
- [ ] Conflict badge in the telemetry bar dynamically reflects the active count of unresolved quarantined files.
