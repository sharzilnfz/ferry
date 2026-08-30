# 08: Initialization guard in TUI, GUI, and web UI

**What to build:** No surface can register an uninitialized directory into
sync. The folder module exposes one directory inspection answering "is this an
initialized Ferry folder". The TUI and GUI folder-addition flows invoke it
before dispatching registration and block with an inline banner pointing at
`ferry init` or `ferry pair`. The web UI's existing check delegates to the same
inspection instead of its own. All three surfaces share one source of truth.

**Blocked by:** None (can start immediately).

**Status:** done

- [x] One inspection in the folder module answers initialized-or-not for a path
- [x] TUI blocks uninitialized directory registration with an init-or-pair banner
- [x] GUI blocks uninitialized directory registration with an init-or-pair banner
- [x] The web UI check delegates to the shared inspection
- [x] Unit tests assert rejection states per surface for paths without a Ferry configuration
- [x] Initialized directories register exactly as before
