# T-17: Windows correctness — colon/prefix path components rejected + exec-bit CI fix

Status: done

Two Windows defects; both testable from any host (pure validation logic) or
gated correctly:

1. **Drive-relative components ("C:x") pass every validator and replace the
   whole base path on Windows** (Medium, High on Windows). Both
   `validate_components` (crates/ferry-materialize/src/apply.rs ~1741-1762)
   and `validate_name` (crates/ferry-store/src/manifest.rs ~466-478) reject
   empty/`.`/`..`, separators, NUL, reserved names — but not `:`. On Windows,
   `PathBuf::push` with a prefixed component replaces the entire base
   (documented std behavior), so a remote manifest entry named `C:evil`
   escapes the synced root via `abs_under` (~apply.rs:1452-1466). Local scans
   cannot produce such names (NTFS forbids `:`); this is purely remote input.
   Fix: reject in BOTH validators (defense in depth): any component where
   `Path::new(c).prefix().is_some() || Path::new(c).is_absolute() ||
   c.contains(':')`. Add unit tests with synthetic components — they must run
   on every OS since the logic is pure string handling. Preserve existing
   refusal error kinds/messages for previously-rejected inputs.

2. **Windows CI red: exec-bit assertion.**
   `snapshot::tests::snapshot_captures_tree_contents_and_stores_all_blobs`
   panics at crates/ferry-store/src/snapshot.rs:614 ("exec bit maps to flags
   bit 0") because NTFS cannot store the exec bit. Fix in the same spirit as
   commits caf4f95/3fe146f: assert `run.exec == true` only on unix
   (`#[cfg(unix)]` block or `if cfg!(unix)`), on Windows assert only that the
   entry exists with correct payload. Do NOT weaken what unix verifies.

Acceptance: new colon/prefix rejection tests green on macOS (this host);
existing store/materialize suites green; reasoning recorded here that the
windows job goes green — verified on next CI run after merge (note it, do not
block local acceptance on it).

## Resolution (T-17, wave2/t17)

Both validators now refuse colon-bearing and prefixed/absolute components:
`c.contains(':')` plus a stable stand-in for nightly-only `Path::prefix`
(first parsed component must be `Component::Normal`, which catches `C:x`,
`C:\x`, `/abs`, `\x` wherever Windows path parsing applies). Pure string
logic, so tests run on every host. Previously-rejected inputs keep their
error kinds (`BadComponent` in materialize's `validate_components`,
`InvalidName` in store's `validate_name`). UNC names (`\\s\share`) are
host-dependent at the store layer (on unix `\` is an ordinary byte) but are
caught unconditionally by materialize's backslash rule.

Windows job goes green because: (a) the failing snapshot test no longer
asserts `run.exec` off-unix (NTFS cannot store it; entry presence + payload
still verified), matching convention 3fe146f/caf4f95; (b) the new rejections
are OS-independent asserts that pass identically on windows-2022. To be
confirmed on next CI run post-merge.

Local gates: fmt clean; clippy --workspace --all-targets -D warnings clean;
cargo test --workspace 497 passed / 0 failed (baseline 496 + 1).
