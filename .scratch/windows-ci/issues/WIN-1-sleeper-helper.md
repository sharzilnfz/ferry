# Ticket WIN-1: cross-platform sleeper helper for spawn-based tests

Status: done

Depends on:
Blocks:

## Problem

Seven tests spawn `std::process::Command::new("sleep")`, which does not
exist on stock Windows (works on GitHub runners only because the image
ships coreutils on PATH). Every such test dies with NotFound before
reaching its real assertions:

- crates/ferry-pin/src/pin.rs:343,411,439
- crates/ferry-pin/tests/pin_scenario.rs:453
- crates/ferry-platform/src/procs.rs:208
- crates/ferry-cli/tests/pin_cli.rs:150

Diagnosis: .scratch/windows-ci/diagnosis/sleep-spawn.md

## Required behavior

A shared helper (ferry-platform) that spawns a live, distinct, killable
child process with a readable start token, on every OS. Unix arm keeps
`sleep` verbatim. Windows arm per diagnosis: powershell Start-Sleep.
Self-sleeper via current_exe rejected (libtest owns main; see report).

## Constraints

- Both cfg arms must type-check on ALL platforms (commit 53b9ca3 rule).
- No new crate dependencies.
- ferry-pin already depends on ferry-platform; ferry-cli needs a
  dev-dependency line only.
- Regression proof: the seven tests must FAIL on Windows without the
  helper (they do today) and pass with it; unix behavior byte-identical.

## Acceptance

cargo test -p ferry-pin -p ferry-platform -p ferry-cli green locally on
Windows; clippy -D warnings clean workspace-wide.

## Comments

Landed acf0fa4 (spawn_sleeper in ferry-platform, unix= sleep 30,
windows= powershell Start-Sleep 30, both arms type-check everywhere;
replaced 6 call sites; added ferry-platform dev-dep to ferry-cli). Local
Windows: clippy clean, ferry-platform 26 passed, ferry-pin 27+2 passed,
ferry-cli pin_cli 5 passed (previously 7 NotFound failures). CI windows
runner already had sleep on PATH so the fix is local-repro hardening;
no prod code touched. Follow-up fmt fallout (9a1 long lines) fixed in
c29180d.
