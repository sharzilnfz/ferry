# T-006: Walking skeleton (M0)

Status: ready-for-agent
Depends on: T-002, T-003

Two processes on one machine sync a directory through the store over plain
localhost TCP. Deliberately throwaway transport and no encryption; its job is
to prove store → manifest diff → transfer → materialize end-to-end, and to
force every interface seam into existence early. Replaced by T-008/T-009.

Acceptance: script starts both daemons, touches 50 random files including an
append-heavy log file, asserts convergence within N seconds, tears down.
Runs in CI on macOS/Linux.
