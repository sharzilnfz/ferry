# Diagnosis: `sleep` spawn failures on Windows (Cluster A)

Branch fix/windows-ci. 7 test failures, one root cause:
`std::process::Command::new("sleep")` — no such program on Windows
(NotFound). Example panic: `tests-win.log:490` (procs.rs:211:18).

## 1. Failing assertion per site, and what each test verifies

All six sites fail at the same `.expect("spawn sleeper")` / `.unwrap()` on
`spawn()`; none reach their real assertions.

- `crates/ferry-pin/src/pin.rs:343-346`
  (`stale_pin_detected_from_dead_pid_and_does_not_hold`): spawns `sleep 30`,
  kills it (pin.rs:349), reaps, and asserts the orphaned pid's record is
  `Liveness::Stale` and not holding (pin.rs:355-356) while our own live pid
  is Alive/holding (pin.rs:359-360). Verifies kill-minus-nine semantics of
  stale-pin liveness.
- `crates/ferry-pin/src/pin.rs:411-414`
  (`start_stamps_the_current_process_start_token`): first half verifies
  `PinStore::start` stamps THIS process's birth token (pin.rs:398-405);
  second half uses a killed child to verify the tolerant reader: a legacy
  record with `proc_start_token == None` still loads and expires dead
  writers via existence-only liveness (pin.rs:420-426).
- `crates/ferry-pin/src/pin.rs:439-442`
  (`pid_reuse_is_detected_through_start_time_mismatch`): reads the live
  child's start token (pin.rs:444), then models pid reuse by recording OUR
  token under the child's pid; asserts mismatch => Stale, not holding
  (pin.rs:468-476), plus forged-token => Stale (pin.rs:480-483). Verifies
  anti-pid-reuse detection without waiting for real reuse.
- `crates/ferry-pin/tests/pin_scenario.rs:453-456`
  (`orphaned_writer_leaves_a_stale_pin_that_surfaces_but_does_not_hold`):
  end-to-end: killed child's pid written into a pin record must surface as
  stale — readable, `!holding()`, `!released`, never silently dropped
  (pin_scenario.rs:481-483) — and let plans pass through untouched while
  stale (pin_scenario.rs:494+).
- `crates/ferry-platform/src/procs.rs:208-211`
  (`child_process_token_differs_from_parent_when_visible`): verifies the
  probe itself: a spawned child has its own start token, distinct from the
  parent's (procs.rs:226), with a retry for Linux same-jiffy collisions
  (procs.rs:218-225).
- `crates/ferry-cli/tests/pin_cli.rs:150-153`
  (`stale_pin_surfaces_then_a_new_start_replaces_it`): CLI-level: killed
  child's pid in a record reports `state == "stale"`, `holding == false`
  via `pin status` (pin_cli.rs:176-178), and a new `pin start` succeeds
  where pin-active would refuse (pin_cli.rs:181-183).

## 2. Root cause confirmation

Every site needs the SAME three child properties: (a) a real distinct
process whose start-time token is inspectable WHILE it runs, (b) survival
for the duration of the test until explicitly killed (~30 s headroom), (c)
killability + reapability via `Child::kill/wait`. None need `sleep` itself.

Production Windows path is fully implemented: `process_start_token` on
Windows opens `PROCESS_QUERY_LIMITED_INFORMATION` and reads the creation
FILETIME via `GetProcessTimes` (procs.rs:132-175); unix arms read
/proc/stat (procs.rs:60-64) or sysctl (procs.rs:74-127). Production pin
liveness likewise has a Windows arm: OpenProcess + GetExitCodeProcess
(pin.rs:137-152; ferry-pin/Cargo.toml:25-32). So the defect is confined to
test fixtures: production code is never exercised on Windows because every
fixture dies at spawn.

## 3. Minimal fix sketch

Self-sleeper (current_exe + env gate): NO existing helper in the repo —
the only `current_exe` hit is triage.md:39 itself. Worse, all six sites run
under the libtest harness (three are `#[cfg(test)]` unit tests; the rest
are default-harness integration tests), so `current_exe` re-invokes the
generated harness main we don't control; gating would require spawning with
an `--exact <dedicated-test>` filter, and if that filter ever mismatches,
the "sleeper" exits instantly and the tests fail confusingly (token reads
as None / pid already dead). Rejected as too fragile for the value.

Alternatives: `timeout.exe /t 30` fails with redirected stdin ("Input
redirection is not supported") and is console-host dependent — rejected.
`cmd /c ping -n 31 127.0.0.1` is a gross idiom — rejected.

RECOMMENDED: cfg-split sleeper helper in ferry-platform, e.g.
`pub fn spawn_sleeper(secs: u64) -> io::Result<Child>`:

- `#[cfg(unix)]`: exactly today's `Command::new("sleep").arg(secs)`.
- `#[cfg(windows)]`: `Command::new("powershell").args(["-NoProfile",
  "-Command", &format!("Start-Sleep -Seconds {secs}")])`. Windows
  PowerShell 5.1 ships with every supported Windows and is on PATH on
  GitHub windows-2022 runners; ~0.5 s startup per spawn is negligible next
  to a 30 s budget; `Child::kill` terminates it cleanly.

Feasibility of placement in ferry-platform — dependency directions check:

- ferry-pin -> ferry-platform: direct dependency (ferry-pin/Cargo.toml:11,
  workspace entry Cargo.toml:39); normal deps are visible to unit AND
  integration test targets, so pin.rs tests and pin_scenario.rs can call it
  directly.
- ferry-cli -> ferry-pin -> ferry-platform transitively (ferry-cli/
  Cargo.toml:17), but transitive deps are not nameable from
  tests/pin_cli.rs; minimal change is one dev-dependency line
  `ferry-platform = { path = "../ferry-platform" }` in
  ferry-cli/Cargo.toml [dev-dependencies] (ferry-cli/Cargo.toml:27-28).
- ferry-platform depends on nothing internal, so no cycle.
This matches ferry-platform's stated role as the cross-platform process/
file-semantics crate (procs.rs:1-22; README.md:107).

## 4. Regression risk on other OSes

None if the unix arm stays byte-identical to today's spawn: macOS/Linux
keep `sleep 30`; only a new cfg(windows) arm is added. The repo rule from
commit 53b9ca3 applies: platform-gated logic must type-check on ALL
platforms — 53b9ca3 fixed a `cfg!(unix)` runtime check whose else-arm was
compiled on Windows and referenced a unix-only API (admission.rs). Here the
helper uses only std::process::Command in both arms, so both compile
everywhere; keep it that way (no windows-sys types outside `#[cfg(windows)]`)
and run `cargo clippy --workspace --all-targets` on at least one unix CI job.

## 5. Real bug vs test-only

TEST-ONLY portability defect. No production code spawns `sleep`; the
production start-token probe and pin-liveness checks have complete Windows
implementations (procs.rs:133-175, pin.rs:137-152). The consequence is a
coverage gap, not a shipped defect: pin stale/reuse logic is currently
untested on Windows because fixtures cannot spawn a sleeper. Fixing the
fixture closes the gap; no wire/store format impact (triage.md:37-40).
