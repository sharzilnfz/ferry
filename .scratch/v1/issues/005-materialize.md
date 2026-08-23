# T-005: Materialization

Status: ready-for-agent
Depends on: T-003

Apply a manifest to a tree: write through temp files renamed atomically
(Syncthing temp-name conventions incl. Windows variant), verify every block
hash before rename, handle exec-bit-only permission subset, deletions, and
directory creation order. Never modify a destination file in place.

Acceptance: apply is idempotent (second run is a no-op); kill -9 mid-apply
leaves either old or new state, never a torn file (test with process kill at
randomized points).
