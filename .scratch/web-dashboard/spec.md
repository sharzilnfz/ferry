# Web dashboard API contract (v0)

Everything downstream codes against this document. The HTTP layer lives in
`ferry-daemon` (binary `ferry-sync`), served by `axum` (latest 0.x) on tokio
alongside the existing `SyncEngine`. The UI is static HTML/CSS/vanilla JS
embedded into the binary — no npm/node/build tooling, ever.

## Launch

```
ferry-sync daemon ... --ui 127.0.0.1:8098
```

- `--ui <addr>` is optional; when absent no HTTP listener starts and the
  daemon behaves exactly as before.
- Default bind if `--ui` is given without a value: `127.0.0.1:8098`.
- v0 auth stance: **localhost-bind only, no token.** Startup refuses
  (`--ui` error, exit 2-style usage failure) any address whose IP is not a
  loopback address (127.0.0.0/8, ::1). This is the entire security model;
  do not add auth.

## General rules

- All `/api/*` responses are `application/json` with UTF-8 bodies.
- Error shape everywhere (matches docs/cli-json.md):

  ```json
  { "error": string, "code": string, "hint": string }
  ```

  HTTP status mapping: `400` validation/usage errors, `404` not-found /
  unknown path, `409` precondition failures (`secrets-found`,
  `pin-active`, `already-initialized`, ...), `500` internal (code
  `"internal"`, hint carries the message). Reuse CLI error codes verbatim
  wherever the condition matches a documented one; never invent near-duplicates.
- Success responses embed the exact documents defined in `docs/cli-json.md`
  for the corresponding command. Field names are frozen by that doc's
  stability promise. Handlers call ferry-cli library functions directly
  (they are plain functions taking explicit params) — **never shell out**,
  never duplicate their logic.
- No endpoint triggers a tree rescan or hashing work as a side effect of a
  GET. For `/api/status` specifically: do NOT route through
  `ferry_cli::commands::status` — that function performs a fresh scan with
  hashing (`scan::one_shot`) on every call, violating this rule. Handlers
  reuse CLI library functions everywhere EXCEPT status.

## Endpoints

### GET /

Serves the single-page UI: `crates/ferry-daemon/assets/index.html`
embedded at compile time via `include_bytes!`. Content-Type
`text/html; charset=utf-8`. Additional asset files (CSS/JS) under
`assets/` are embedded the same way and served with correct MIME types
(`text/css`, `text/javascript`, `image/svg+xml`). Unknown paths under `/`
return the index (SPA fallback) except paths starting with `/api/`.

### GET /api/status

Wraps the `ferry status --json` document shape exactly (source of truth:
`docs/cli-json.md`, "ferry status" section). Values come from the
daemon's cached engine state:

- `manifest_id`: last manifest root computed by the engine's most recent
  poll tick — NOT recomputed per request.
- `scanned.*`: counts from that same last poll tick.
- `pending_changes`, `pin.*`, `held_changes`, `held_by_peer`, `peers`,
  `conflicts`: read from `.ferry/` state files (agreements.json-class,
  pin-state, held ledgers, conflicts.jsonl) — cheap metadata reads only.
  Exception: `pending_changes` is NOT computable as a pure metadata read;
  the blessed v0 implementation is the root-equality approximation
  (0 = agreed root equals current root, -1 = they differ or the agreed
  manifest is unreadable, null = no agreement). Do not solve it with
  per-request scans or per-request store opens; cache the verdict.

If no poll has completed since startup, return `503` with
`code: "warming-up"` until the first tick lands. Never block on a store or
engine lock held across I/O; snapshot under a short lock, render outside it.

### GET /api/conflicts

Embeds the `ferry conflicts list --json` document exactly
(`command`, `folder`, `entries[]` per docs/cli-json.md). Read from
`.ferry/conflicts.jsonl` per request (cheap line reads).

### POST /api/share

Request body:

```json
{ "folder": string | null, "i_know": bool }
```

`folder` null/omitted = the daemon's configured folder. Behavior mirrors
`ferry share [folder] [--i-know]`:

- Success `200`: the share initiate document (same shape as `pair`
  initiate with `"command": "share"`) including the payload path and
  empty-or-populated `warnings`.
- Findings exist and `i_know` is false: `409`, code `secrets-found`,
  body gains `warnings: [...]` in the documented redacted shape.

### POST /api/pair/accept

Request body:

```json
{ "payload_path": string | null, "dir": string | null }
```

Mirrors `ferry pair --accept <file> [dir]`. Success `200`: the accept
document from docs/cli-json.md (`role: "accept"`). Errors map to the
documented codes (`bad-offer`, `already-initialized`, `not-found`, ...)
with statuses per the general rules.

### POST /api/pin/start · /api/pin/stop · /api/pin/release

All three take the same request shape:

```json
{ "folder": string | null, "paths": [string] | null }
```

`paths` non-null only meaningful for `start` (equivalent of `--paths`;
null pins the whole folder). Each returns the matching document from the
`ferry pin start|stop|release` sections of docs/cli-json.md. Errors
(`pin-active`, `bad-pattern`, ...) map per the general rules (`pin-active`
→ 409).

### GET /api/events

Server-Sent Events stream of daemon STATE lines. One event per engine
state transition, plus an initial replay of the current state on connect:

```
event: state
data: STATE root=<hex> agreed=<hex|none>

```

The data payload is the same machine-greppable `STATE root=... agreed=...`
line the daemon prints today, byte-for-byte. The stream must be produced
by observing engine state changes — it MUST NOT poll the tree, hold any
engine lock across the connection, or allocate per idle client.

Polling fallback: clients may ignore SSE entirely and re-fetch
`/api/status`; the UI should degrade to polling (2s) if the EventSource
errors twice. If SSE threatens scope during this session, ship polling
only and note it in the ticket comments — `/api/events` may then return
`501` with `code: "not-implemented"`.

## Non-goals (v0)

- Auth tokens, HTTPS, remote binds, CORS beyond same-origin defaults.
- Any write path other than share/pair-accept/pin (no init/add/delete via
  HTTP).
- Multi-folder dashboards (the daemon currently serves one folder).
