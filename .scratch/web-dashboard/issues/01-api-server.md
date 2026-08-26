Status: done
Depends on:
Blocks: 04-e2e-live.md, perf-findings-web.md review

# 01 — `--ui` HTTP server in ferry-daemon

Add an axum-based HTTP listener to the `ferry-sync` binary so the daemon
can serve the web dashboard alongside the sync engine.

## Requirements

1. New flag `--ui <addr>` (default `127.0.0.1:8098` when given bare) in
   `crates/ferry-daemon/src/main.rs`'s hand-rolled arg parsing (follow
   existing `flag()`/`require()` patterns; no clap here).
2. Refuse any non-loopback bind address at startup with a clear usage
   error (v0 auth stance per `.scratch/web-dashboard/spec.md`: localhost
   only, no token).
3. Serve the axum app on tokio alongside `SyncEngine`. The engine runs on
   its own threads today (`engine.start()` + `join_until_signal()`); run
   the axum server on a tokio runtime in a separate thread and shut down
   cleanly on signal (reuse the existing signal path if practical; a
   detached thread that dies with the process is acceptable for this
   session).
4. Implement every route in `.scratch/web-dashboard/spec.md`:
   - `GET /` + embedded assets from `crates/ferry-daemon/assets/`
     (`include_bytes!`; create a minimal placeholder `index.html` stub if
     ticket 02 hasn't landed yet — 02 replaces it wholesale)
   - `GET /api/status` — cached engine state, NEVER triggers a rescan;
     shape = `ferry status --json` document per docs/cli-json.md
   - `GET /api/conflicts` — `ferry conflicts list --json` document
   - `POST /api/share`, `POST /api/pair/accept`,
     `POST /api/pin/start|stop|release` — request/response shapes exactly
     as spec.md defines
   - `GET /api/events` — SSE stream of `STATE root=<hex> agreed=<hex|none>`
     lines; polling fallback allowed if SSE threatens scope (return 501
     code "not-implemented" instead, note it in ticket comments)
5. Handlers call ferry-cli library functions directly (plain functions,
   explicit params) or the lib APIs those functions call. NO shell-outs.
6. Error responses use the spec's `{error, code, hint}` shape with the
   documented status mapping.
7. `/api/status` must snapshot under short locks only — never hold an
   engine/store lock across I/O or across the SSE connection.

## Constraints

- Files: `crates/ferry-daemon/*` ONLY (Cargo.toml already has axum/tokio/
  serde_json wired by the orchestrator — do not edit workspace Cargo.toml
  or other crates).
- No new dependencies beyond what's already declared.
- No npm/node tooling. Assets are plain files compiled in via include_bytes!.
- clippy clean: `cargo clippy -p ferry-daemon --all-targets -- -D warnings`.
- No code comments unless essential.

## Verify

```
cargo clippy -p ferry-daemon --all-targets -- -D warnings
cargo build -p ferry-daemon
ferry-sync daemon --transport tcp --role listen --store /tmp/ui-a/store \
  --tree /tmp/ui-a/tree --tag a --poly <genpoly output> --ui 127.0.0.1:8098 &
curl -s localhost:8098/api/status | python3 -m json.tool
curl -si localhost:8098/ | head -5
```

## Comments

(orchestrator) API contract lives at `.scratch/web-dashboard/spec.md`;
code against that document, not against this ticket's paraphrase.

(build agent, 2026-08-26) Landed. `--ui [ADDR]` (default `127.0.0.1:8098`)
parses through the existing hand-rolled helpers; non-loopback binds are
refused at startup with a usage error and exit 1. The axum app runs on a
two-worker tokio runtime in its own thread; bind errors surface
synchronously before the thread detaches.

Routes shipped:

- `GET /` plus `/index.html`, `/style.css`, `/app.js` embedded via
  include_bytes! with `text/html`, `text/css`, `text/javascript`
  (charset on html/css). Unknown non-API paths serve the index; unknown
  `/api/*` paths return JSON 404.
- `GET /api/status` — cached engine state only: `manifest_id` comes from
  the engine's current folder pointer under a short internal lock;
  pin/held/conflicts/peers are `.ferry/` metadata reads in
  spawn_blocking. No rescans, no hashing, no store locks across I/O.
- `GET /api/conflicts` — conflicts.jsonl lines verbatim per request.
- `POST /api/share`, `POST /api/pair/accept` — real pairing ritual via
  ferry-crypto (offer/response/grant files beside the offer, CONFIG_HEAD
  wrap entries, store adoption), 120s waits like `ferry pair`.
- `POST /api/pin/start|stop|release` — start/stop write the exact
  ferry-pin pin-state.json schema so the engine's hold filter keeps
  working; release with nothing held completes fully, see cut below.
- Errors use `{error, code, hint}` with the spec status mapping; CLI codes
  reused verbatim.

Deferred (both noted here so nobody hunts for them):

- SSE `/api/events` returns `501 not-implemented`; the bundled UI already
  falls back to 2s polling after two EventSource errors.
- `pin/release` with actual held ledger entries also returns `501
  not-implemented`: reconciliation needs ferry-sync-engine's three-way
  plan/execute, which this binary cannot link without a Cargo.toml change.
  Run `ferry pin release` on the command line instead. The nothing-held
  case is pure bookkeeping and works.

Known approximations (engine accessors don't exist yet):

- `manifest_id` carries the cached root-tree blob id from
  `EngineHandle::root_id()`, not the manifest object id; it matches the
  daemon's printed STATE root.
- `pending_changes` is `null` (no agreement), `0` when the current root
  equals the most recent agreement's root, else `-1`. Full diffs need the
  engine's snapshot.
- `scanned.*` comes from a one-shot metadata-only walk (no hashing),
  computed once on first request then cached; `bytes_chunked` stays 0.
- Pin liveness is pid-approximated (no ferry-pin platform tokens here): a
  pin reads "active" only while its declared pid is this daemon's pid.

One constraint conflict worth recording: the ticket says handlers call
ferry-cli functions directly, but ferry-cli is not a dependency of
ferry-daemon and Cargo.toml edits were forbidden. Handlers re-implement
the flows on ferry-crypto/ferry-store primitives instead, mirroring the
CLI code path by hand. If a later ticket links ferry-cli into the daemon,
share/accept should switch to calling it outright.

Verification: `cargo clippy -p ferry-daemon --all-targets -- -D warnings`
clean; `cargo build -p ferry-daemon` clean; `cargo test -p ferry-daemon`
2 passed, `cargo test -p ferry-cli` all suites pass. Live boot on
127.0.0.1:8098 (`--transport tcp`) answered /api/status with the full
status document whose manifest_id matched the daemon's STATE line,
served / and /style.css with correct MIME types, returned the 501 JSON
for /api/events, 404 JSON for unknown API paths, and completed a
pin start → stop → release cycle over POST with contract-shaped bodies.
The warming-up 503 gate is implemented but hard to catch live: the engine
runs its first poll tick immediately at startup, so the window is
milliseconds.
