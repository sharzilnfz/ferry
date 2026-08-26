# Ticket WIN-3: record local-env requirements (no code)

Status: done

Depends on:
Blocks:

## Finding (verified by CI run 32969906200)

Two local-only failure clusters do NOT reproduce on the GitHub windows-
2022 runner: the image ships sleep(1) on PATH and enables Developer Mode
(all 107 store-lib, 34 materialize-lib, kill_safety, pin suites green).

Requirements for a clean Windows dev box to run the suite locally:
- Developer Mode ON (symlink tests; Os error 1314 otherwise) — admin or
  Developer Mode required by std::os::windows::fs::symlink_*.
- A sleep(1)-equivalent on PATH, until WIN-1 lands.

## Task

Append these findings to triage.md ## Comments and close this ticket.
NO production code changes, NO test weakening, NO doc-file edits beyond
the scratch tracker (README semantics are owned elsewhere).

## Comments

Verified by CI runs 32969906200 (pin_enforcement red, adversarial hidden)
and 32980463581 (all green). GitHub windows-2022 images have Developer
Mode ON and sleep(1) on PATH, so three local-only failures (apply
idempotent, symlinks_created, type_changes — Os 1314) and seven NotFound
sleep spawns do not reproduce on the runner. Local dev boxes without
DevMode/admin or without coreutils on PATH will see them; triage.md
documents this. No production fix needed for local-only cluster beyond
WIN-1 (sleep) and the symlink mtime fix (039db0d) which was required for
CI determinism even with DevMode (adversarial_fixture). Ticket closed.
