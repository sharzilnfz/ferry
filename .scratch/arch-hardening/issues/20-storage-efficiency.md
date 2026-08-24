# T-20: Storage-efficiency sweep — bounded on-disk growth, no orphaned bytes

Status: ready-for-agent
Depends on: T-14, T-15 (exchange paths settled so the steady-state write set
is final)

Cross-cutting audit directive: Ferry's value prop is "carries everything
else" — its own metadata must not quietly become the biggest thing in the
folder. One pass over every persistent writer, fixing waste without format
changes (ADR-0001 settled):

1. **Orphaned temp residue.** Every crash-recoverable write site must have a
   reclamation story: stale `*.tmp` / `pull-*` leftovers in packs/, pins,
   quarantines, materialize temp dirs. Sweep-on-startup (bounded, older-than)
   or tie into existing open/init paths; document where each lives.
2. **Unreferenced pack retention.** After manifests move forward, superseded
   packs stay forever. Add a simple mark-from-live-manifests GC command or
   daemon-idle pass (explicit user action acceptable; never auto-delete
   within N hours; quarantine ADR-0004 semantics untouched). If a full GC is
   too big, ship the reachability report (`ferry store gc --dry-run`) plus
   the delete path behind it.
3. **Manifest/tree churn.** Confirm unchanged subtrees reuse tree ids across
   snapshots (no per-scan tree rewrite); if scans mint duplicate identical
   tree nodes, dedupe by content hash (should already hold — prove it with a
   test asserting repeated scans of an unchanged folder grow the store by
   ~zero bytes).
4. **Index/state duplication.** `.ferry` sidecars (agreement ledger, peer
   records from T-18, pin state): compact-on-threshold rather than
   append-forever where the format allows without compat break; tolerant
   readers already exist per T-10.
5. **Test hygiene.** No committed binary fixtures >100 KiB without
   justification; integration tests use tempfile (auto-cleaned), never the
   repo tree.

Acceptance: test proving an unchanged folder re-scanned K times grows the
store below a small constant; dry-run GC report test shows superseded packs
listed and live packs never listed; startup sweep removes planted stale temp
files; full workspace suites green.
