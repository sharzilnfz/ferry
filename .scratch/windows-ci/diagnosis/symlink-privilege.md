# Cluster B diagnosis — Os error 1314 symlink creation (fix/windows-ci)

Written 2026-08-26. Research only; no code changed.

## 1. What production code does on Windows when materializing a symlink entry

`Mutation::WriteSymlink` handling, crates/ferry-materialize/src/apply.rs:690-738:

1. **Policy re-check** — `ferry_platform::classify_link(depth, &target)`; absolute or
   root-escaping targets return `LinkDecision::Refuse` → `MaterializeError::SymlinkRefused`
   (apply.rs:694-704). Only relative in-tree targets proceed.
2. **Windows dir-link gate** — `reject_windows_dir_link(&abs, ...)` (apply.rs:705,
   defined apply.rs:768-801): if the lexically resolved target is an existing real
   directory inside the tree AND `!cfg!(windows) || allow_windows_dir_links()` fails,
   it errors with `WindowsDirLinkRefused`. The env knob
   `FERRY_ALLOW_WINDOWS_DIR_LINKS=1` is read by `ferry_platform::allow_windows_dir_links()`
   (crates/ferry-platform/src/links.rs:107-116), default OFF everywhere.
3. **Temp + rename dance** (apply.rs:707-727): any directory occupying the path is torn
   down children-first (apply.rs:709-714); the link is created at
   `parent.join(temp_name_for(...))`, then atomically renamed onto `abs`; on error the
   temp file is removed (apply.rs:721-726); the parent dir is fsynced (apply.rs:727).
4. Link's own mtime restored via `set_symlink_times` when present (apply.rs:731-736).

The actual creation call, `make_symlink(target, at)` (apply.rs:1822-1851):
- unix: `std::os::unix::fs::symlink` (apply.rs:1825).
- windows: picks flavor from whether `at.parent().join(target)` currently resolves to a
  dir — `std::os::windows::fs::symlink_dir` (apply.rs:1838) else
  `std::os::windows::fs::symlink_file` (apply.rs:1840). Comment at apply.rs:1829-1832:
  "Creating symlinks on Windows needs developer mode/admin; failure surfaces loudly."
- No privilege flags are passed anywhere. This is plain std — no windows-sys, no
  `SYMBOLIC_LINK_FLAG_*`. std internally passes `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`
  (which is what makes Developer Mode sufficient), but ferry does not set or check anything;
  without Developer Mode/admin the OS returns ERROR_PRIVILEGE_NOT_HELD = **1314**, which
  propagates as `io_at(at, e)` → test `.unwrap()` panics.

A second, independent copy of the same dance exists in the sync engine:
crates/ferry-sync-engine/src/execute.rs:338-376 (`materialize` at tmp via
`make_symlink`, execute.rs:441-470, same std calls, same comment at execute.rs:451).
Cluster B failures all funnel through one of these two.

## 2. Reconciling "symlinks refused loudly" in docs

Two distinct things are documented; they are not contradictory:

- **Policy refusal (hostile targets)** — SPEC.md:58-61 "explicit symlink policy
  (default: sync as links where safe, refuse dangerous cases loudly)";
  README.md:117-118 "symlink policy refuses anything that could escape the sync
  root". This is implemented purely in `classify_link`
  (ferry-platform/src/links.rs:72-98): absolute / drive-letter / `..`-escaping targets
  are refused loudly at scan and materialize. It says nothing about local privilege.
- **Privilege reality (creating links locally)** — README.md:127 platform table:
  Windows CI ✅ but "symlinks require Developer Mode or admin";
  links.rs:21-27 documents that ANY Windows symlink creation needs developer mode/admin
  (citing research/landscape.md:88, which cites Unison/Mutagen precedent) and defines the
  junction/dir-link escape hatch; apply.rs:1831-1832 repeats it.
- docs/store-format.md:459-461 ("refused loudly") is again about target *encoding*
  (non-UTF-8 targets refused at scan), not privilege.

So: refusal semantics are about WHICH links sync (policy); local symlinks already on
disk are scanned/stored normally (`symlink_metadata` reads never need privilege, e.g.
apply.rs:583, naming.rs:77), and re-materializing safe links on Windows simply requires
Developer Mode — a documented environmental requirement, not an intended refusal.

## 3. Is Developer Mode ON on GitHub windows-2022 runners?

Claim is plausible but I could not confirm from repo evidence alone; do not treat it as
fact until verified empirically. Reasoning from documented runner-image behavior:
GitHub's windows-2022 image includes Developer Mode among installed features (the
actions/runner-images README lists it), which is why unprivileged symlink creation
generally works on hosted Windows runners. But images change; verify against run
32969906200 once the `test (windows-2022)` job (job id 98180938269) finishes:

```
gh run view --log --job=98180938269
```

Decisive signal: cluster B tests pass ⇒ privilege held (Developer Mode ON). If they fail
with os error 1314 in the log ⇒ claim false and this becomes cluster-real-bug. At write
time the job was still running (clippy green; Tests step in progress).

## 4. Decision recommendation

**If CI is green** (failures were local-environmental only): no code change. Document
the local-env requirement instead — triage.md already records it (.scratch/windows-ci/
triage.md:18-20,52-55); optionally add one line to README.md:127's note pointing
contributors at Developer Mode for running the workspace tests locally.

**If CI reproduces 1314** (real defect): minimal correct handling is a **preflight
privilege probe that downgrades only those specific assertions** — NOT blanket
skip-with-loud-message of production behavior, and NOT skipping symlink coverage
(forbidden by triage.md:53-54 and README.md:127's first-class status):

- Precedent already in-tree: crates/ferry-sync-engine/tests/adversarial_fixture.rs:106-129
  probes once per process (`symlink_creation_works`: try creating+removing a probe link)
  and gates link assertions on the result (adversarial_fixture.rs:194). Mirror that
  helper into ferry-materialize/ferry-store test modules and gate ONLY the six failing
  tests' symlink-entry expectations behind it, emitting a loud
  `eprintln!("skipping symlink assertions: host lacks SeCreateSymbolicLinkPrivilege …")`.
- Production code stays untouched and still fails loudly (apply.rs:1842) — the
  documented contract (README.md:114-119) is about manifests and policy fidelity, not
  about hosts being able to create links; a loud skip preserves zero-silent-loss.
- Do NOT convert this into a runtime "skip symlinks if no privilege" path in apply.rs:
  that would silently drop data on constrained endpoints, violating PRODUCT.md:87 /
  links.rs:17-19 ("nothing silently dropped").

## 5. Cross-platform risk per option (must type-check on macOS/Linux)

| Option | Risk |
|---|---|
| A. No change (CI green, document env) | Zero code risk. Doc-only. |
| B. Preflight probe gating test assertions | Low. Probe helper must be written with `#[cfg(unix)]`/`#[cfg(windows)]`/fallback arms exactly like adversarial_fixture.rs:112-129 (already compiles on all three OSes today). Gated tests keep compiling; only assertions branch. Watch: probe file cleanup on failure paths; OnceLock pattern avoids repeated 1314s. cfg(not(any(unix,windows))) arm keeps other targets green. |
| C. Skip-with-loud-message in PRODUCTION apply | High risk: silently weakens contract on privileged-vs-unprivileged hosts; also engine copy (execute.rs:441) would need same change — drift between two make_symlinks. Reject. |
| D. windows-sys direct call w/ privilege flags | Medium-high: new unsafe FFI dep, diverges from execute.rs copy, no benefit over std's built-in flag. Reject. |

Recommendation order: A if CI green, else B.

---
Summary lines follow in reply.
