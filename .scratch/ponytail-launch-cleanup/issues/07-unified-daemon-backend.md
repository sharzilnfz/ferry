# 07: Unified daemon backend FolderBackend

Status: ready-for-agent
Depends on: 01
Blocks: 08, 09

**What to build:** One backend for the daemon UI seam so adding a new endpoint means editing one implementation and fixing a transport error once fixes it everywhere. From the dashboard and TUI user perspective `get_status` and `share`/`pair` behave identically whether the daemon is local or remote, with transparent fallback. From the maintainer perspective `is_transport` fallback logic lives at one site.

**Blocked by:** 01

**Status:** ready-for-agent

- [ ] The triplication of `AutoBackend`, `DirectBackend`, and `InProcessAdapter` is replaced by one `FolderBackend` parameterized over a state source that provides folder open, list, pin state, and event streaming; the existing backend trait remains the boundary per `boundary-discipline`
- [ ] Ten copy-paste `is_transport` fallback arms in the IPC backend are collapsed to one helper or macro site; transport errors map uniformly rather than per-endpoint
- [ ] The dashboard server composes the single backend directly; the supervisor no longer branches on which backend variant is active
- [ ] Verified through the backend contract seam: contract tests and daemon backend tests assert that status, share, pair, and folder registration produce identical results via daemon and local paths, and the picker `not-initialized` guard is uniform across TUI, GUI, and daemon

## Comments

Tracer-bullet but the widest slice in this feature. 1 244 lines become about 500. Follows `minimize-reader-load`: one place to add an endpoint. If this ticket proves too large for one context window, split it as 07a introduce `FolderBackend` beside the old trio and 07b migrate callers and delete the trio, both blocked by 01.
