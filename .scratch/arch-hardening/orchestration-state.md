# Orchestration state — arch-hardening

Updated: 2026-08-25 (post repo relocation). Read this BEFORE doing anything else,
then follow the reading order in AGENTS.md (spec → CONTEXT → ADRs → flakes → tickets).

## Current state — everything committed and verified

- Repo lives at `~/Projects/dumps/idea2` (MOVED OFF Desktop on 2026-08-25 to escape
  macOS TCC folder protection; the old `access()` EPERM / linker failures are gone
  for good — no shims or workarounds exist anymore, none needed).
- Branch `arch-hardening`, HEAD `64e3467` ("docs: mark tickets 07, 10, 13 done"),
  on top of:
  - `e2a1b46` Merge wave6/t13 (T-13 NFC fold cache)
  - `2bf614b` Merge wave6/t10 (T-10 canonical last-agreed codec + ledger)
  - `e08e2b6` merge of arch-hardening into wave6/t10 (conflict resolution)
  - `6496645` Merge wave5/t07 · `8c33d85` Merge wave5/t11
- GATES GREEN at HEAD, run natively in the new location 2026-08-25:
  fmt ✅ clippy ✅ **535 passed / 0 failed** (cold build).
- All ticket branches are fully merged (`git branch --no-merged arch-hardening`
  is empty); no stashes; fsck clean.

## Ticket status

DONE (Status: done in each file): T-01 … T-09, T-11, T-12, T-13, T-16, T-17, T-19.

REMAINING (all `ready-for-agent`, no blockers):
- **T-18** TOFU peer policy — worktree ALREADY PREPARED:
  `$TMPDIR/opencode/wt/t18` on branch `wave6/t18` @ `e2a1b46`, clean, nothing
  launched yet. Its dependency T-07 is merged. Launch first.
- **T-14** retire CLI M0 stack (deps T-06 ✅ + T-07 ✅)
- **T-15** store contention relief ∥ **T-20** storage-efficiency sweep (Wave 8)

## Final wrap-up checklist (after the last ticket merges)

1. All tickets Status: done.
2. `scripts/quickstart-e2e.sh`, `skeleton-e2e.sh`, `adversarial-fixture.sh` pass.
3. `docs/cli-json.md` schema unchanged vs pre-hardening.
4. Set `.scratch/arch-hardening/spec.md` Status → done.
5. Delete this state file once everything above is true.

## Execution conventions (established, keep using)

- Orchestrator-only merging; one background sub-agent per ticket, own worktree at
  `$TMPDIR/opencode/wt/tNN`, branch `waveN/tNN` off current head.
- Merge with `--no-ff`, message `Merge waveN/tNN: T-NN <slug>`; full gates
  (fmt/clippy `-D warnings`/test) after every merge before launching next.
- Agent commits start `T-NN:`. Agents get: ticket path, worktree path, git-status
  check, std-threads/no-new-deps/no-format-change constraints, ADRs settled,
  frozen CI (do not touch .github), revert-and-report rule, storage directive
  (tempfile tests, bounded RAM/disk, `cargo clean` as agent's final step),
  flake protocol from flakes.md (one wall-clock failure under load → re-run
  isolated; known load-flakes: pin_enforcement,
  empty_peer_hydrates_whole_tree_from_scratch).
