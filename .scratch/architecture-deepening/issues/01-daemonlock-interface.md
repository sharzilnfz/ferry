# 01: DaemonLock interface with deadline and PID preservation

**What to build:** `ferry daemon stop` and `ferry daemon status` become honest.
The platform module gains one lock interface that owns the daemon PID file:
acquire, read the PID, check liveness without PID-reuse confusion (using the
existing process start token), and terminate with backoff polling up to a
five-second deadline. Stop deletes the PID and socket files only after the OS
confirms the process has exited. On timeout, stop reports an error and
preserves the PID file, so a following status reports the live PID. The PID
filename is spelled in exactly one place. Stop and status become pure functions
of a directory, so tests run against a temp Ferry home without spawning a real
daemon.

**Blocked by:** None (can start immediately).

**Status:** done

- [x] `daemon stop` polls with backoff up to a five-second deadline and verifies exit via the OS
- [x] On timeout, stop exits with an error, preserves the PID file, and status reports the live PID
- [x] PID and socket files are unlinked only after confirmed process exit
- [x] PID parsing, liveness probing, and termination live in one interface on the platform module; the CLI no longer calls the OS process API directly
- [x] The daemon PID filename appears in exactly one place
- [x] Stop and status are testable against a temp home with zero real daemon processes
- [x] Stop returns unambiguous exit codes and output payloads a CI script can assert
- [x] Existing daemon tests still pass; new lifecycle tests assert observable behavior only
