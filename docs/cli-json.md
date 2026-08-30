# CLI JSON output (`--json`)

Every `ferry` command accepts a global `--json` flag. The contract:

- **stdout carries exactly one JSON document** per invocation (the daemon
  emits newline-delimited event objects; see below). Human progress and
  diagnostics go to **stderr**, in both modes.
- **Errors** print one JSON object to **stderr** and exit nonzero:
  `{ "error": string, "code": string, "hint": string }`. Some codes add
  extra keys (documented per command). Exit codes: `1` generic failure,
  `3` secrets-found, `2` clap usage errors.
- **Stability promise**: existing field names are never renamed or removed;
  types never change; new fields may be appended; enumerated string values
  are listed here and only ever extended. These schemas are pinned by
  checked-in snapshot tests (`crates/ferry-cli/tests/expected/`).

Types below use JSON Schema-ish shorthand: `string`, `int`, `bool`,
`null`, `[T]` array of T, `{...}` object.

## Global

| Flag | Meaning |
|---|---|
| `--json` | machine output on stdout |
| `-v/--verbose` | more stderr detail |
| `--version` | version string |

## `ferry init [path]`

```json
{
  "command": "init",
  "folder": string,             // as given on the command line
  "folder_id": string,          // 32 lowercase hex
  "device_id": string,          // 64 lowercase hex (X25519 public key)
  "created": true,
  "ignore_file_created": bool
}
```

## `ferry pair`

Initiate (`ferry pair`, after the acceptor completes):

```json
{
  "command": "pair",
  "role": "initiate",
  "status": "completed",
  "folder": string,
  "folder_id": string,
  "peer_device_id": string,
  "short_code": string,        // XXXX-XXXX-XXXX-XXXX-XXXX (canonical alphabet)
  "offer_file": string
}
```

Accept (`ferry pair --accept <file> [dir]`):

```json
{
  "command": "pair",
  "role": "accept",
  "status": "completed",
  "folder": string,
  "folder_id": string,
  "device_id": string,
  "expected_short_code": string
}
```

Error codes: `pair-timeout` (response/grant file never appeared),
`pair-bad-response`, `pair-verify`, `bad-offer`, `bad-grant`,
`already-initialized`, `not-found`. These paths can also fail with
`config-corrupt` (folder key envelope missing/damaged),
`not-shared-with-device`, `key-unwrap`, `store`; `share` additionally
emits `identity-corrupt`, and both may emit the generic `io`, `crypto`,
and `qr` codes.

## `ferry share <folder> [--i-know]`

Same document as `pair` initiate with these differences: `"command"` is
`"share"`, plus:

```json
{
  "warnings_reviewed": bool,   // true iff findings existed and --i-know was given
  "warnings": [
    {
      "path": string,          // '/'-joined relative path
      "line": int | null,      // 1-based; null for path-level findings
      "class": string,         // see below
      "preview": string        // redacted: first 4 chars + length
    }
  ]
}
```

Warning classes (extensible): `env-file-included`, `private-key-file-included`,
`credentials-json-included`, `npmrc-included`, `aws-access-key`, `openai-key`,
`github-token`, `slack-token`, `private-key-header`,
`generic-credential-assignment`.

When findings exist and `--i-know` is absent, the command fails with code
`secrets-found` (exit 3); the error document gains a `warnings` array with
the same shape above. Previews are always redacted.

## `ferry status [folder]`

```json
{
  "command": "status",
  "folder": string,
  "folder_id": string,
  "device_id": string,
  "manifest_id": string,       // fresh scan; 64 hex
  "scanned": { "files": int, "dirs": int, "symlinks": int, "bytes_chunked": int },
  "pending_changes": int | null,
                             // vs most recent agreement; null = no agreement yet;
                             // -1 = agreement exists but its manifest is unreadable
  "pin": {
    "state": "none" | "active" | "stale" | "released",
    "holding": bool,           // true only while an ACTIVE pin actually holds
    "paths": [string]
  },
  "held_changes": int,         // distinct held paths across all peers
  "held_by_peer": { "<peer device_id>": [string, ...] },
  "peers": [
    {
      "device_id": string,
      "last_agreed_manifest_id": string | null,
      "agreed_at": string | null,     // RFC 3339 UTC
      "connectivity": "reachable" | "unreachable" | "unknown"
    }
  ],
  "conflicts": int                    // entries in conflicts.jsonl
}
```

Connectivity is best-effort TCP reachability of the address recorded by an
earlier daemon/sync run (`.ferry/peers/<peer>.addr`); without one it is
`unknown`.

## `ferry conflicts list`

```json
{
  "command": "conflicts",
  "folder": string,
  "entries": [
    {
      "ts": string,                  // RFC 3339 UTC
      "folder_id": string,
      "path": string,
      "kind": "both_changed" | "delete_vs_edit" | "add_vs_add",
      "winner": { "device": string, "mtime_sec": int | null, "mtime_nsec": int | null },
      "loser":  { "device": string, "mtime_sec": int | null, "mtime_nsec": int | null },
      "quarantined_as": string | null
    }
  ]
}
```

Entries mirror `.ferry/conflicts.jsonl` lines exactly (ferry-sync-engine
schema, oldest first).

The log compacts on threshold: past 4096 entries an append atomically
drops the oldest lines down to 1024. The quarantined files the entries
point at are never touched — only this report is capped.

## `ferry store gc [folder] [--dry-run] [--grace-secs SECONDS]`

Mark-from-live-manifests pack collection for the folder's store.
Explicit user action only — Ferry never deletes packs on its own. Liveness
roots are every last-agreed manifest recorded for this folder plus every
held-change manifest still awaiting `ferry pin release`; the chunker
polynomial's pack is always live. Packs younger than `--grace-secs`
(default 86400) are never deleted, so in-flight writers and just-published
manifests are safe. Quarantined conflict copies are ordinary tree files
(ADR-0004) and are untouched.

With `--dry-run`: read-only reachability report:

```json
{
  "command": "store",
  "action": "gc",
  "folder": string,
  "dry_run": true,
  "scanned_packs": int,
  "live_packs": int,
  "garbage_packs": [
    { "pack": string,            // 64 lowercase hex
      "bytes": int }             // on-disk size
  ],
  "reclaimable_bytes": int,
  "skipped_corrupt": [string]
}
```

Without `--dry-run`, fully-unreachable packs past the grace period are
deleted; younger ones are only recorded (their grace clock starts here):

```json
{
  "command": "store",
  "action": "gc",
  "folder": string,
  "dry_run": false,
  "scanned_packs": int,
  "deleted": [string],           // deleted pack ids, 64 hex
  "recorded_unreferenced": int,
  "skipped_corrupt": [string]
}
```

Error codes: `not-a-folder`, `store-open`, `store`, `agreement-state`.

## `ferry ignore`

Append (`ferry ignore '<pattern>'`):

```json
{
  "command": "ignore",
  "action": "added-line",
  "pattern": string,
  "folder": string,
  "preset": null,
  "rules_file": string
}
```

Preset (`--preset claude|opencode`):

```json
{
  "command": "ignore",
  "action": "applied-preset",
  "preset": string,
  "folder": string,
  "description": string,
  "rules_file": null
}
```

List (`--list`; layer order = precedence order, lowest first):

```json
{
  "command": "ignore",
  "action": "list",
  "folder": string,
  "honor_gitignore": bool,
  "applied_presets": [string],
  "layers": [
    { "name": string, "lines": [string] }
  ]
}
```

Error codes: `unknown-preset`, `bad-pattern`.

## `ferry sync [folder] --peer-url HOST:PORT`

```json
{
  "command": "sync",
  "folder": string,
  "folder_id": string,
  "device_id": string,
  "peer_device_id": string | null,
  "converged": bool,
  "rounds": int,
  "chunks_sent": int,
  "chunks_received": int,
  "ops_applied": int,
  "quarantined": int,
  "conflicts_recorded": int,
  "held": int
}
```

Exit code is `0` when `converged` is true, `1` otherwise (best-effort
semantics).

## `ferry daemon [folders...]`

Long-running. In `--json` mode stdout carries newline-delimited event
objects, one per completed exchange round:

```json
{
  "event": "round",
  "folder": string,
  "folder_id": string,
  "peer_device_id": string | null,
  "roots_equal": bool,
  "meta_fetched": int,
  "chunks_sent": int,
  "chunks_received": int,
  "ops_applied": int,
  "quarantined": int,
  "conflicts_recorded": int,
  "held": int,
  "agreed": bool
}
```

Human mode prints the machine-greppable line `LISTENING <addr>` after
binding (`--listen`) for scripts, plus per-round summaries on stderr.

Error codes: `transport-unavailable` (any `--transport` value other than
`tcp`; iroh QUIC lands with T-009/T-014), `bind`, `bad-address`.

## `ferry daemon stop`

```json
{"command": "daemon", "action": "stop", "status": "stopped", "pid": int}
{"command": "daemon", "action": "stop", "status": "not_running"}
```

`stopped` exits 0 after the OS confirms the daemon process exited; only
then are the PID and socket files unlinked. `not_running` exits 0 and
still clears a stale socket file. If the daemon outlives the five-second
stop deadline the command fails with code `daemon-stop-timeout` (exit 4)
and the PID file is preserved, so a following `ferry daemon status`
reports the live PID.

## `ferry daemon status`

```json
{"command": "daemon", "action": "status", "status": "running", "pid": int, "socket": string}
{"command": "daemon", "action": "status", "status": "stopped"}
```

`running` means the PID file records a process the OS confirms is the
same instance that wrote it (start-token check, so a reused PID is
reported as `stopped`). Exits 0 either way; liveness is not an error.

## `ferry pin start|stop|release|status`

While pinned, the exchange loop holds competing remote edits to the pinned
paths instead of applying them (session pinning, T-015). Held decisions are
ledgered one line each under `.ferry/held/<peer>.jsonl`:

```json
{
  "held_sec": int, "held_nsec": int,
  "path": string,                        // '/'-joined stored path
  "device_id": string,                   // 64 hex peer whose change this is
  "remote_manifest_id": string,          // 64 hex manifest that carried it
  "chunks": [ { "id": string, "len": int } ],
  "decision": "remote_apply" | "remote_delete" | "conflict",
  "conflict_winner": null | "local" | "remote"
}
```

`ferry pin start [--paths <glob>...] [folder]` (no `--paths` pins the whole
folder, equivalent to `*`):

```json
{
  "command": "pin",
  "action": "start",
  "folder": string,
  "device_id": string,
  "pid": int,
  "paths": [string],
  "started_at": string,                  // RFC 3339 UTC
  "base_peers_recorded": int             // last-agreed bases frozen at start
}
```

`ferry pin stop [folder]` — ends the session WITHOUT reconciling; ledgers
stay on disk and `release` still recovers them later:

```json
{
  "command": "pin",
  "action": "stop",
  "folder": string,
  "was_pinned": bool,
  "held_changes": int,
  "held_by_peer": { "<peer device_id>": int }
}
```

`ferry pin release [folder]` — replays every ledger through the ordinary
three-way engine (base = last agreement captured at pin start; a peer never
agreed reconciles against an empty ancestor). Winners stay live, losers are
quarantined `path.ferry-conflict.<device>-<ts>`, conflicts.jsonl gains one
entry per conflict. Ledgers clear only after their plan executed, so a
failed release is always retryable and nothing is discarded implicitly:

```json
{
  "command": "pin",
  "action": "release",
  "folder": string,
  "peers": [
    {
      "device_id": string,
      "remote_manifest_id": string,
      "held_entries": int,
      "held_paths": [string],
      "ops_applied": int,
      "quarantined": int,
      "conflicts_recorded": int
    }
  ],
  "quarantined": int,
  "conflicts_recorded": int,
  "ops_applied": int,
  "pin_ended": bool,
  "conflicts_total": int                 // entries now in conflicts.jsonl
}
```

`ferry pin status [folder]`:

```json
{
  "command": "pin",
  "action": "status",
  "folder": string,
  "state": "none" | "active" | "stale" | "released",
  "device_id": string | null,
  "pid": int | null,
  "started_at": string | null,
  "paths": [string],
  "holding": bool,
  "held_changes": int,
  "held_by_peer": { "<peer device_id>": [string, ...] }
}
```

A STALE pin means its recorded writer process no longer runs (crash or
reboot). Nothing is held while stale — incoming edits apply — but the
marker stays on disk until an explicit `start` replaces it or `stop`
discards it. It surfaces in `status`, never silently expires.

Error codes: `pin-active` (start while another session holds), 
`bad-pattern`, `pin-state-corrupt`, `held-ledger-corrupt`,
`held-manifest-missing`, `structural-split`, `pin-release-reconcile`,
`store`.

## Per-folder settings file

`<folder>/.ferry/settings.json` (CLI-owned, not part of the store format
contract):

```json
{
  "format_version": 1,
  "folder_id": string,
  "honor_gitignore": bool,
  "presets": [string],
  "overrides": [string]
}
```
