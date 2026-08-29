# Windows CI triage — fix/windows-ci

Reproduced locally 2026-08-26 on win32 (msvc, rustc 1.98.0), branch
fix/windows-ci @ origin/arch-hardening tip 4292124.

- `cargo clippy --workspace --all-targets -- -D warnings`: **green** (3m32s)
- `cargo test --workspace --no-fail-fast`: **11 failures / 8 binaries**, all
  in two clusters below.
- Full logs: `clippy-win.log`, `tests-win.log` (repo root, untracked).

## Environment notes (affect reproduction fidelity)

- Machine had NO C++ toolchain at session start; VS Build Tools 2022
  (MSVC 14.44 + SDK 10.0.26100) installed this session via winget.
- rustup default was flipped msvc→gnu mid-session by an unknown process at
  11:56–11:58; reset to `stable-x86_64-pc-windows-msvc` to match CI
  (`dtolnay/rust-toolchain@stable` on windows-2022). CI parity requires msvc.
- Developer Mode OFF, shell not elevated → symlink creation fails with
  Os error 1314. GitHub windows-2022 runners ship Developer Mode ON, so
  cluster B may be local-only; CI push is the decisive experiment.
- Git longpaths enabled globally.
- Uncommitted `opencode.json` permission changes stashed (not ours, not
  committed): stash "opencode.json permission changes…".

## Cluster A — unix-only `sleep` spawn in tests (REAL bug, test-only)

7 failures, one root cause: tests do `std::process::Command::new("sleep")`,
which does not exist on Windows ("program not found", NotFound).

- crates/ferry-pin/src/pin.rs:343 (stale_pin_detected_from_dead_pid_and_does_not_hold)
- crates/ferry-pin/src/pin.rs:411 (start_stamps_the_current_process_start_token)
- crates/ferry-pin/src/pin.rs:439 (pid_reuse_is_detected_through_start_time_mismatch)
- crates/ferry-pin/tests/pin_scenario.rs:453 (orphaned_writer_leaves_a_stale_pin…)
- crates/ferry-platform/src/procs.rs:208 (child_process_token_differs_from_parent_when_visible)
- crates/ferry-cli/tests/pin_cli.rs:150 (stale_pin_surfaces_then_a_new_start_replaces_it)

Classification: real portability defect in TEST code only; production pin
logic itself untested on Windows because fixtures can't spawn. Fix sketch:
spawn the test binary against itself (current_exe + env-gated sleep mode)
or a cfg-split sleeper helper. No wire/store impact.

## Cluster B — symlink creation needs privilege (environmental here)

6 failures, root cause Os error 1314 (SeCreateSymbolicLinkPrivilege):
Developer Mode off + non-admin shell.

- crates/ferry-materialize/src/apply.rs:2222, :2391, :2457 (lib tests)
- crates/ferry-materialize/tests/kill_safety.rs:492, :521 (harness shells out
  to examples/apply_once.rs which panics at apply.rs symlink creation)
- crates/ferry-store/src/snapshot.rs:651 (fixture creates a symlink)

Classification: environmental on THIS machine. GitHub runners have
Developer Mode ON. Do NOT "fix" by skipping symlink coverage on Windows —
README documents symlinks as first-class manifest entries. If CI shows the
same failure, escalate to real-bug track instead.

## Not reproduced (handoff said surviving)

Handoff 755a899 named "windows mtime restore" and "ubuntu pin-token flake".
Commits 2d8f5ba..4292124 (post-handoff) appear to have landed fixes; local
run shows no mtime failures on Windows. Ubuntu pin-token flake cannot be
reproduced locally; watch CI runs for recurrence and label if flaky.
