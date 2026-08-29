Status: done
Depends on: 01-api-server.md, 02-web-ui.md
Blocks:

# 04 — End-to-end live test of dashboard sync

Write `scripts/dashboard-e2e.sh`: prove the full loop — two daemons with
UI servers, one file dropped via filesystem A, byte convergence in both
trees, green agreement in both HTTP status responses.

## Requirements

1. Script style follows the existing scripts (`scripts/quickstart-e2e.sh`
   is the reference: temp dirs, strict mode, colored pass/fail, cleanup
   trap).
2. Flow:
   - genpoly once; boot node A (`--role listen`, tcp transport, distinct
     addr) with `--ui 127.0.0.1:<port-a>`; boot node B (`--role connect`)
     with `--ui 127.0.0.1:<port-b>`.
   - Poll `/api/status` on both until each reports non-warming-up state.
   - Write a file with known bytes into tree A.
   - Wait (with timeout) until both trees contain the file AND
     `cmp`-identical bytes.
   - Assert both `/api/status` responses show agreement (agreed root
     equal/non-null per whatever /api/status exposes; consult
     .scratch/web-dashboard/spec.md).
   - Also exercise at least one POST round-trip: e.g. `/api/pin/start`
     then `/api/pin/stop` on node A returning success documents.
3. Kill daemons on exit via trap; nonzero exit + diff-style diagnostics
   on failure; print a final PASS line listing both UI URLs.
4. Keep total runtime bounded (~60s timeout ceiling).

## Constraints

- File: `scripts/dashboard-e2e.sh` ONLY (new file). Do not modify Rust
  code — report bugs you find instead.
- No new dependencies beyond curl + python3 (already assumed present)
  for JSON assertions.
- bash, POSIX-ish, macOS-compatible (this machine is macOS).

## Verify

```
bash scripts/dashboard-e2e.sh
```

It must pass end-to-end on your run. Report the exact commands and output
tail in Comments when done.

## Comments

### Agent report (integration) — PASS

`bash scripts/dashboard-e2e.sh` passes end-to-end (ran 4x: 4/4 green,
2–3s each, well under the 60s ceiling). macOS bash 3.2 compatible; only
curl + python3 beyond POSIX tools. `shellcheck` clean except SC2329
(false positive: cleanup is invoked via trap, same shape as
quickstart-e2e.sh).

Flow implemented: genpoly -> node A (`--transport tcp --role listen`,
random loopback addr + `--ui 127.0.0.1:<p1>`) -> node B (`--role connect`
+ its own `--ui 127.0.0.1:<p2>`) -> poll both `/api/status` out of 503
warming-up -> write known bytes into tree A -> bounded wait for the file
to exist in tree B AND `cmp -s` identical -> settle-loop on both status
docs -> POST `/api/pin/start` then `/api/pin/stop` on node A -> PASS line
with both UI URLs. Cleanup trap kills daemons and removes the temp dir
(`FERRY_KEEP=1` keeps logs for debugging).

Exact assertions against `/api/status` (per node, both must hold):

- HTTP 200 after warming-up (503 with `code: "warming-up"` before first
  tick — confirmed live, matches spec).
- `manifest_id` present and matches `^[0-9a-f]{64}$`.
- `pending_changes == 0` on BOTH nodes (settled agreement).
- `peers` non-empty; every entry's `last_agreed_manifest_id` is hex64.
- Cross-node: `manifest_id(A) == manifest_id(B)` — equal roots over
  byte-identical trees.

POST round-trip assertions: pin/start → HTTP 200, `command=pin`,
`action=start`, integer `pid > 0`, `paths == ["*"]`; pin/stop → HTTP 200,
`action=stop`, `was_pinned: true`.

Output tail of a passing run:

```
== assert agreement green in BOTH /api/status documents
agreement OK: roots equal, pending_changes=0, peers present on both nodes
  root A/B: 72b4abe735d5d97d77b3bd98098f67878bd3fb2cf75907cdfc03dd844e754694
  peers A: ['dc6ba1ee']
  peers B: ['74f87550']
== POST round-trip: /api/pin/start then /api/pin/stop on node A
pin/start OK: {"action": "start", "base_peers_recorded": 1, "command": "pin", ...}
pin/stop OK: {"action": "stop", "command": "pin", ..., "was_pinned": true, ...}
PASS: dashboard e2e converged in 3s
PASS: node A dashboard http://127.0.0.1:24248
PASS: node B dashboard http://127.0.0.1:38313
```

Notes for downstream tickets:

- The agreement assertion polls (bounded ~24s window) rather than sampling
  once right after `cmp` passes. Reproduced manually: immediately after
  byte convergence, one node can still report `pending_changes: -1` while
  its ledger records the newest agreement (node A log showed
  `STATE scan discarded: adoption landed mid-scan`). It settles to 0 on
  both sides within ~1s and stays there. Not filed as a bug — the engine
  exposes no way to distinguish "agreement in flight" from "disagreement",
  which is exactly why `pending_changes: -1` exists per the comment in
  crates/ferry-daemon/src/ui/status.rs:22.
- Confirmed against the live daemon: `/api/events` answers 501
  `not-implemented`; `/api/share` returns `"warnings": []` always (secret
  scanner not linked, per crates/ferry-daemon/src/ui/actions.rs:291).
- Minor doc drift, not bugs: the two nodes record DIFFERENT
  `last_agreed_manifest_id`s (each side stores its own manifest object id;
  roots are what agree). Anything asserting cross-node equality on that
  field will fail.

