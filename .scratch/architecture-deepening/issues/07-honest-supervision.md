# 07: Honest supervision

**What to build:** The supervisor restarts engines on real failure. Engine
health is surfaced from the engine handle into the supervision tick, so a
crashed engine is observed and restarted with the existing exponential backoff.
The placeholder sleep-loop task, its abort helper, and its finished-check helper
are deleted. Restart accounting stays internal to the supervisor; no test
asserts on task handles.

**Blocked by:** None (can start immediately).

**Status:** ready-for-agent

- [ ] A genuinely crashed engine is detected by the tick and restarted with backoff
- [ ] A crash of one engine does not disturb other supervised engines
- [ ] The fake sleep-loop task, abort helper, and finished-check helper are deleted
- [ ] Restart tests assert observable behavior (engine revived, others untouched), never task handles
- [ ] Existing supervisor tests pass with the fake task removed
