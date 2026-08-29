# T-08: Quarantine landing must be create-exclusive

Status: done

`unique_conflict_dest` (crates/ferry-sync-engine/src/naming.rs:55-78) probes
with symlink_metadata, but the rename happens much later in
execute.rs/write_loser_copy — cross-process executors (CLI sync while daemon
runs) can both resolve the same conflict name free and the second rename
silently OVERWRITES the first loser copy, destroying data ADR-0004 says must
never be lost.

Fix: make landing create-exclusive — e.g. retry loop of rename-unless-exists
(unix: link/renameat2 RENAME_NOREPLACE if available via nix/libc, else open
target with O_CREAT|O_EXCL as a reservation probe immediately before each
rename), or reserve-at-resolution. Keep it simple and portable; Windows needs
a working story too (CreateFile with CREATE_NEW as the probe). On collision,
regenerate the candidate name and retry (bounded attempts).

Acceptance: a test simulates a racing writer by pre-creating the chosen
destination between resolution and landing and asserts the loser copy lands
under a fresh unique name with original bytes intact (never overwritten);
existing quarantine tests green.
