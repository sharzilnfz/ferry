# T-010: Three-way reconciliation and conflict quarantine

Status: ready-for-agent
Depends on: T-003, T-005

Track last-agreed manifest per folder per peer. Reconciliation = three-way
merge between local tree, remote manifest, and last-agreed base (Mutagen's
model). Divergent paths quarantine per ADR-0004: winner stays live, loser
becomes `path.ferry-conflict.<device>-<ts>`, structured report entry.
Deletion-vs-edit conflicts resurrect with a marker.

Acceptance: exhaustive test matrix (both-change, delete-vs-edit, add-vs-add
same content, add-vs-add different content) across two simulated devices;
zero silent data loss in any scenario; `ferry conflicts list` shows every
quarantine.
