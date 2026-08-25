# T-10: One canonical last-agreed codec + one store

Status: done
Depends on: T-07 (folder-pointer ownership settled so consumers are known)

Three independent encodings of the same spec record (peer id + manifest id +
timestamp + flags, required byte-exact across devices):
- ferry-sync/src/state.rs encode_agreed_record/parse_agreed_record +
  AgreementStore
- ferry-proto/src/agreement.rs to_canonical/from_canonical + AgreementLedger
- ferry-sync-engine/src/agree.rs serialize_agreed_record/parse_agreed_record
  + PeerStateStore

Drift here breaks agreement silently (both sides must derive identical bytes).
Consolidate: pick ONE canonical byte format (the one docs/store-format.md
specifies — follow the spec document, not whichever implementation is
convenient) and ONE storage module living in the lowest-dependency crate that
needs it (likely ferry-proto next to the agreement concept, or a leaf module
in ferry-store if persistence belongs there). Other crates import it; delete
the two redundant codecs and stores, migrating readers/writers of existing
on-disk records (compatibility: if the three formats differ on disk today,
write a migration-or-tolerant-reader path and note it).

Acceptance: golden-bytes test pins the canonical encoding; rg confirms a
single parse/serialize pair; cross-restart baseline-recovery tests read real
files written by the surviving implementation; status/agreement flows in CLI
still work (status --json schema unchanged).
