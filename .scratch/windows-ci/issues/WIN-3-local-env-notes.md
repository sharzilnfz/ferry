# Ticket WIN-3: record local-env requirements (no code)

Status: ready-for-agent
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
