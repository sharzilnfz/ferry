# T-13: NFC live-fold: cache per-parent listings; loud error on duplicate spellings

Status: ready-for-agent
Depends on: T-09 (apply.rs streaming rewrite lands first)

In ferry-materialize/src/apply.rs, abs_under/live_nfc_match resolve EVERY
component with symlink_metadata + full read_dir + per-entry NFC normalization
— O(N*depth*dirsize) syscalls per multi-path apply. Worse, when a directory
genuinely contains both normalization spellings of one name, live_nfc_match
silently picks the lexicographically smaller raw name; guards then report
phantom ExpectedAbsent/divergence and the whole apply aborts confusingly.

Fix:
1. Cache the NFC-fold map per parent directory for the duration of one apply
(invalidate nothing mid-apply — the applier owns its own writes; entries it
creates itself go straight into the cache).
2. Replace silent min()-pick with an explicit typed error ("ambiguous disk
spelling: <nameA> and <nameB>") so operators see the real problem; guards
refusing loudly beats phantom divergences.

Acceptance: syscall-count regression guard is hard; instead assert
correctness + performance structurally: unit test that resolving M children
of one parent performs at most one read_dir (inject a counting fs handle or
structure the cache so it's directly observable); duplicate-spelling fixture
produces the typed ambiguity error, not phantom divergences; existing
nfd_disk_spelling tests stay green.
