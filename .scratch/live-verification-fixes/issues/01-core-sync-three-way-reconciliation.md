# 01: Core Sync Three-Way Reconciliation & Unpinned Conflict Quarantine

Status: done
Depends on: None
Blocks: 09-e2e-live-process-and-browser-verification.md

**What to build:**
When two unpinned devices concurrently modify the same file and initiate an exchange, Ferry must evaluate the changes using three-way reconciliation against their last-agreed base manifest rather than performing a blind overwrite. The revision with the newer modification timestamp (or higher device identifier on tiebreak) remains live in the working tree, while the losing revision is preserved in a designated conflict quarantine file adjacent to the winner and recorded in the persistent conflict ledger.

**Blocked by:** None (can start immediately)

### Acceptance Criteria

- [x] Core synchronization exchange invokes three-way reconciliation against the last-agreed manifest base before applying remote changes to the working tree.
- [x] Concurrent edits to the same file between two unpinned devices preserve the winning file in place and write the losing file to a conflict quarantine path (`<path>.ferry-conflict.<loser-device>-<timestamp>`).
- [x] Every quarantined conflict generates an immutable entry in the persistent conflict report ledger (`conflicts.jsonl`) detailing file path, conflict type, winner device, loser device, and timestamp.
- [x] Simultaneous edit-versus-delete conflicts resurrect the edited file rather than allowing it to vanish silently.
- [x] Identical content modifications differing only in timestamps or executable permissions resolve deterministically without creating duplicate conflict files.
- [x] Automated regression tests verify that no unpinned concurrent modification ever results in silent overwrites or unrecorded file loss.
