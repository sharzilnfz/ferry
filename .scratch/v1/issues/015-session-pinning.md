# T-015: Session pinning prototype (M4)

Status: ready-for-agent
Depends on: T-010, T-013

`ferry pin start|stop`: while pinned, a device declares active-writer status;
competing remote edits to paths the local tree changed since pin are held and
surfaced (`ferry status` shows held set) instead of racing. Prototype scope:
single peer, manual release. This is the agent-writes-overnight story from
research archetype 7 — the feature no competitor has.

Acceptance: scripted scenario where device A pins, mutates files, device B
mutates the same paths concurrently; B's changes hold until release; release
produces explicit conflicts per ADR-0004, never torn writes.
