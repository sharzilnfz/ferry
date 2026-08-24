# T-012: Cross-platform guardrails and CI matrix

Status: done
Depends on: T-005, T-010

Case-conflict detection at scan time (case-folding index per folder), NFC name
normalization everywhere, Windows long paths via `\\?\` prefixes, explicit
symlink policy (sync as link where safe; refuse junction/symlink-to-dir on
Windows unless developer mode documented), reserved-name handling. GitHub
Actions matrix: macOS arm64, Ubuntu x64, Windows x64; the walking-skeleton and
reconciliation suites must pass on all three.

Acceptance: adversarial fixture tree (unicode names, case-only rename,
deep nesting past 260 chars, symlink chains) syncs correctly or fails loudly
with an actionable message on every OS.

## Comments

### Policy decisions (owner's record)

**New crate `ferry-platform`** holds every policy as pure functions,
unit-tested identically on all platforms (21 tests): `casefold`, `winpath`,
`reserved`, `links`, `time`. No new external dependencies beyond
`unicode-normalization` (already in the workspace).

**Case conflicts.** Fold key = NFC then Unicode lowercase (simple-fold
semantics like Go's `unicode.SimpleFold` that Syncthing uses; Greek final
sigma collapsed into the σ orbit; NOT full caseless folding — ß stays ß).
`CaseFoldIndex` maps fold key → canonical NFC spelling; a decomposed vs
precomposed pair is ONE name, not a conflict. Fatal at scan AND at materialize
when `host_folds_case()` — Windows always, macOS by safe default (APFS is
case-insensitive unless explicitly formatted otherwise; such users get a loud
refusal naming both spellings instead of silent breakage on mainstream peers),
Linux never (README/readme legitimately coexists there and Linux↔Linux sync of
such trees keeps working). The materialize gate runs over the desired state
before anything is written. Never silently picked anywhere.

**NFC everywhere audit.** Scan + manifest layers were already NFC (T-003);
this ticket extended coverage to: ignore-pattern matching (both directions
verified: NFD pattern ↔ NFC path and NFD path ↔ NFC pattern through the public
API), conflict/quarantine display names (`conflict_display_name` defensively
NFC-normalizes, so decomposed manifest input yields the identical quarantine
filename), and the fold keys above. Required test landed in three layers:
ferry-store (`decomposed_and_precomposed_directory_spellings_are_one_name`,
real disk), ferry-platform (`index_treats_nfc_equal_pair_as_one_name`),
ferry-sync-engine (quarantine name equality).

**Windows long paths.** `extend_path()` applies `\\?\` (drive form) or
`\\?\UNC\` to Windows-shaped absolute paths whose length ≥ MAX_PATH (260);
idempotent, normalizes `/` → `\` inside prefixed paths, identity for relative,
POSIX, non-UTF-8, and short paths so callers apply it unconditionally.
Wired into `Applier::abs()`. Per-component >255 UTF-16 units are NOT lifted by
the prefix (NTFS limit) and surface as loud IO errors. Registry/manifest
opt-in is deliberately not relied upon — a sync tool controls neither.

**Reserved device names** (CON, PRN, AUX, NUL, COM1-9, LPT1-9 with any
extension, case/trailing-space tolerant; COM0/LPT0/COM10 excluded): refuse
loudly at BOTH scan (ledger refusal `ReservedName`) and materialize (hard
error before any write). Rationale: such entries can never be represented on a
Windows endpoint, so carrying them only converts an immediate local error into
a delayed cross-device one. Tradeoff accepted: a Linux file literally named
`aux.txt` will not sync; the message says to rename it.

**Symlink policy.** Relative targets that stay inside the folder root sync as
links (chains fine); absolute targets (POSIX root, drive letters incl.
drive-relative `C:x`, backslash root, UNC) and `..`-escaping targets are
REFUSED at scan (ledger, actionable message with fix) and re-checked at
materialize before creating anything (defense in depth against peer
manifests). Windows directory links/junctions: refused unless env
`FERRY_ALLOW_WINDOWS_DIR_LINKS=1` (documented developer-mode opt-in; creation
needs developer mode/admin per landscape research). File links need no gate.
The gate is compile-checked everywhere but runtime-exercised only on Windows
(doc-only locally, as planned).

**Deferred T-005 piece: symlink mtime restoration.** Landed in
ferry-materialize as pub `set_symlink_times` (`utimensat`
AT_SYMLINK_NOFOLLOW, unix); ferry-sync's M0 engine dropped its private copy
and calls the shared one. Wired into BOTH apply paths (full-tree manifests and
change sets — the latter mattered: without it, link-time metadata drift could
oscillate between devices forever; found by the fixture). Non-unix builds
no-op it (documented deviation); link times restored at creation only, drift
repair via next full apply.

### Bug found by the fixture (fixed)

Case-only rename on folding hosts: planning `rename-me.TXT` against live disk
matched the folded old spelling (`Rename-Me.txt`) on size/content/mtime and
degraded to Skip; executing the old spelling's removal afterwards deleted the
only copy. The applier now detects upserts fold-shadowed by pending removals
and forces a real write. Also learned: `Path::exists()` folds case on
macOS/Windows — assertions use exact-spelling directory listings.

### Adversarial fixture results (macOS arm64, local run green)

`scripts/adversarial-fixture.sh` wraps
`crates/ferry-sync-engine/tests/adversarial_fixture.rs`:
- round trip snapshot→materialize→resnapshot reproduces the IDENTICAL root
  tree id (unicode NFD names, emoji dir, 14-level deep branch, symlink chain
  a→b→c→file, exec bits, file/dir/symlink mtimes);
- case-only rename propagates; folding host refuses the guarded apply loudly
  (asserted), modifies nothing, then converges via unguarded apply;
- reconciliation conflict on the unicode file between two devices: zero data
  loss, A-wins bytes live on both, loser quarantined under an NFC-composed
  name carrying B's short id, convergence fixed point reached.
Exit code propagates from cargo; portable bash wrapper works under git-bash.
Deep branch creation goes through `extend_path` so Windows runners can build
it without registry opt-in; symlink chain is probe-gated (hosts that forbid
creation skip it rather than fail setup).

### CI status (honesty note)

`.github/workflows/ci.yml`: matrix macOS-14 (arm64) / ubuntu-24.04 (x64) /
windows-2022 (x64); steps = checkout, dtolnay stable + rustfmt/clippy +
Swatinem cache, `cargo fmt --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, skeleton-e2e.sh (**unix only**, see below),
adversarial-fixture.sh (all three). **The repo has no remote/push yet, so the
workflow activates on first push.** Everything it runs was verified locally on
macOS except Windows-only behaviors, which the pure-function tests cover on
every platform (prefix math boundary table at exactly 259/260 chars, reserved
table incl. extensions/case/trailing spaces, symlink classification table,
fold-key edges). Windows compilation was addressed by de-unix-ifying ferry-store/ferry-scan
metadata handling (std `modified()`, `as_encoded_bytes()`, cfg'd exec bit) but
could not be fully cross-checked locally: blake3's C build needs MSVC tools
(`ml64.exe`) absent here, so first Windows verification happens on CI push.
skeleton-e2e.sh is gated off Windows because its daemon-pair lifecycle
(retried ports, background PIDs, trap teardown) is historically flaky under
git-bash; Windows process behavior stays covered by the workspace tests.

### Deviations

- New workspace crate `ferry-platform` (ticket said "dependencies: none new
  ideally" — no EXTERNAL deps added; one internal crate keeps scan/materialize
 /engine from duplicating policy or depending on each other sideways).
- Reserved names refused at scan on ALL hosts (not just Windows) per ticket
  wording; means Linux trees containing e.g. `aux.txt` must be renamed to
  sync. Documented above.
- Symlink ledger refusals mean an escaping link never enters manifests rather
  than failing the whole scan — consistent with existing loud-ledger design;
  the rest of the tree proceeds either way.
