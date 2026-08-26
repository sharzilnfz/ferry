Status: done
Depends on: .scratch/web-dashboard/spec.md (API contract)
Blocks: 04-e2e-live.md

# 02 — Single-page web dashboard UI

Build the dashboard front-end as static files under
`crates/ferry-daemon/assets/`. Served by the binary from ticket 01;
embedded via include_bytes!, so plain HTML/CSS/vanilla JS only — no
frameworks, no build step, no node_modules.

## Read first

- `PRODUCT.md` Users section: solo multi-machine devs plus agent
  wranglers reviewing from a laptop. The UI is a developer tool, not a
  marketing page.
- `.scratch/web-dashboard/spec.md` for every endpoint's exact shapes.

## Requirements

1. Single page, dark, minimal, developer-tool aesthetic (think terminal /
   system-monitor density, not consumer SaaS). System font stack is fine.
2. Views/data:
   - Folder overview: device id, folder id, manifest root, last agreement
     state — from `/api/status`.
   - Peers list with connectivity + last-agreed info (from `/api/status`).
   - Agreement state rendered clearly: green when agreed root matches,
     amber/none otherwise.
   - Conflicts list from `/api/conflicts`, empty-state text when clean.
   - Share button → `POST /api/share` (shows payload path + any secret
     warnings; offers "share anyway" → retry with `i_know: true`).
   - Pair accept → `POST /api/pair/accept` (payload path + dir inputs).
   - Pin start/stop/release controls → `/api/pin/*` (optional paths input).
3. Live state via SSE `/api/events` (`event: state`,
   `data: STATE root=... agreed=...`). Degrade to polling `/api/status`
   every 2s after two EventSource errors. If ticket 01 shipped polling-
   only (SSE returns 501), poll from the start.
4. All fetches handle the `{error, code, hint}` body: surface `code` +
   `hint` inline near the action that failed. Never swallow errors.
5. Keep total assets small (a few files, tens of KB). index.html may be
   self-contained (inline CSS/JS) or split into style.css/app.js — your
   call; no other files needed.

## Constraints

- Files: ONLY new/replaced files in `crates/ferry-daemon/assets/`. If a
  placeholder index.html exists from ticket 01, replace it wholesale.
- Do NOT touch Rust sources, Cargo.tomls, or other crates' files.
- Use CONTEXT.md terminology exactly (folder, manifest, peers, agreement,
  conflicts, pinning).
- No comments in JS beyond where genuinely essential.

## Verify

Serve the assets with any static file server (e.g.
`python3 -m http.server`) pointed at the assets dir and eyeball layout +
error states with fetches mocked or failing gracefully; integration
happens in ticket 04.

## Comments

### Build report (ticket 02)

**Files created** (all under `crates/ferry-daemon/assets/`, ~21 KB total):

- `index.html` — single page: header (live-conn indicator + agreement
  badge), live STATE strip, folder overview grid, peers list, conflicts
  list, share / pair-accept / pinning action cards.
- `style.css` — dark developer-tool theme (`#0d1117` family), system UI
  stack + `ui-monospace` for all ids/hex/values. Terminal density:
  small type, bordered rows, pill badges.
- `app.js` — vanilla JS, no deps. Syntax-checked via JavaScriptCore
  (`new Function(src)`); served with `python3 -m http.server`, all three
  files returned 200 with correct MIME types.

**Layout:** one column, max-width 860px. Sections top-to-bottom mirror
read priority: folder (device id, folder id, manifest root, pending,
held, scanned counts, pin state pill) → peers (connectivity dot +
green "agreed ✓" badge when `last_agreed_manifest_id === manifest_id`,
amber "not agreed" otherwise) → conflicts (entries reversed newest-first,
dashed empty state when clean) → share → pair accept → pinning.

**SSE vs polling:** boots by fetching `/api/status`; if it answers, opens
`EventSource('/api/events')` and renders each `event: state` line's
`root=` / `agreed=` into a header badge (green in-sync when equal, amber
none/diverged). Two EventSource errors close the stream and start a 2s
`setInterval` poll of `/api/status` — this also covers ticket 01 shipping
poll-only with `/api/events` → 501. If the very first status fetch fails
with `warming-up` (503), a 2s retry loop holds off opening SSE until the
daemon answers. Conn indicator shows live/polling/off at all times.

**Error surfaces:** every fetch goes through one helper that parses the
`{error, code, hint}` body and throws it; each action card has its own
inline error box showing `code` bold + `error` + dimmed `hint`. Non-JSON
or empty bodies still surface as `http-<status>` with the raw text as
hint; fetch-level failures become a synthetic `network` error — nothing
is swallowed. Share-specific: a `409 secrets-found` response renders its
`warnings[]` as a redacted table (path/line/class/preview) plus an
enabled "share anyway (i_know)" button that retries with `{i_know:true}`;
the button stays hidden for any other failure. Pin release refreshes both
status and conflicts on success.

**Terminology** matches CONTEXT.md exactly (folder, manifest, peers,
agreement, conflicts, pinning); field names come straight from
docs/cli-json.md including the `pending_changes` null/-1 semantics.

