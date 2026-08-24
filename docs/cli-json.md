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

## `ferry init [path]` / `ferry add <path>`

```json
{
  "command": "init" | "add",
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
`already-initialized`, `not-found`.

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
  "conflicts_recorded": int
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
  "agreed": bool
}
```

Human mode prints the machine-greppable line `LISTENING <addr>` after
binding (`--listen`) for scripts, plus per-round summaries on stderr.

Error codes: `transport-unavailable` (any `--transport` value other than
`tcp`; iroh QUIC lands with T-009/T-014), `bind`, `bad-address`.

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
