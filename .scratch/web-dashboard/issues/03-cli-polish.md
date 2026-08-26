Status: done
Depends on:
Blocks:

# 03 — CLI polish for `--ui` + JSON docs

Prepare `ferry-cli` for the dashboard landing on top of it.

## Requirements

1. Document the new `--ui` flag in the `ferry daemon` AFTER_HELP text
   (crates/ferry-cli/src/cli.rs): what it does, default address,
   localhost-only v0 stance, pointer to `.scratch/web-dashboard/spec.md`.
   Note: `--ui` itself is implemented in ferry-daemon (separate ticket);
   this ticket only documents it where users read about daemon flags —
   and ensures the flag name doesn't collide with anything clap already
   claims.
2. Audit docs/cli-json.md against the actual command outputs the UI will
   consume (`status`, `conflicts list`, `share`, `pair --accept`,
   `pin start|stop|release`). Fix drift: any field the code emits that
   the doc lacks, or vice versa. The doc is contract; the snapshot tests
   in crates/ferry-cli/tests/expected/ are ground truth. Do NOT rename or
   remove existing fields (stability promise) — append-only if the doc
   was missing something real, otherwise fix the doc's description.
3. While wiring: fix small rough edges you find in the JSON paths of
   those commands ONLY if they are cheap, behavior-preserving for humans,
   and don't change documented shapes (e.g. a missing hint string). List
   anything bigger under Comments instead of doing it.

## Constraints

- Files: `crates/ferry-cli/*` ONLY.
- No new dependencies.
- Snapshot tests must stay green; update expected files ONLY if the doc
  audit proves current output wrong AND the change is additive to JSON.
- clippy clean: `cargo clippy -p ferry-cli --all-targets -- -D warnings`.

## Verify

```
cargo test -p ferry-cli
cargo clippy -p ferry-cli --all-targets -- -D warnings
```

## Comments

Implemented in `crates/ferry-cli` only. Verify: `cargo test -p ferry-cli`
(all green) and `cargo clippy -p ferry-cli --all-targets -- -D warnings`
(clean).

**AFTER_HELP**: added `DAEMON_AFTER_HELP` const in cli.rs, attached via
`#[command(after_help)]` on the `Daemon` variant — documents `--ui
[HOST:PORT]`, default `127.0.0.1:8098`, loopback-bind-only v0 stance, no
auth token, pointer to `.scratch/web-dashboard/spec.md`. No collision: no
existing arg or alias claims `ui` (globals are `--json`/`-v`; daemon has
`listen`, `peer-url`/`peer`, `transport`, `interval-secs`). The flag
itself is still implemented in ferry-daemon (separate ticket).

**Drift found and FIXED (code, additive)**: docs/cli-json.md promises the
`secrets-found` error document gains a `warnings` array, but share.rs set
`CliError.detail` to a bare JSON array and main.rs only merges *object*
details into stderr JSON — so under `--json` the warnings were silently
dropped. Fixed share.rs to emit `{ "warnings": [...] }`; updated
tests/share_gating.rs to read through the new key. Error-document shape
is now as documented and strictly additive.

**Drift found and FIXED (doc, append-only)**: pair/share error-code list
was missing real codes emitted on those paths (`config-corrupt`,
`not-shared-with-device`, `key-unwrap`, `store`, `identity-corrupt`,
generic `io`/`crypto`/`qr`). Appended; nothing renamed or removed.

**Audited clean against fixtures/ground truth** (no changes needed):
status, conflicts list, share success document, pair accept,
pin start/stop/release/status — all match docs/cli-json.md and
tests/expected/*.schema.txt exactly.

**Deferred findings (bigger than a rough-edge fix, not done)**:
- `pin release` human text slices peer ids with `.get(..8)` after an
  `as_str().unwrap()` chain (commands/pin.rs) — safe for current shapes
  but brittle; would prefer typed structs over json! round-tripping.
- `status` connectivity probe blocks up to 500ms per peer sequentially;
  a dashboard polling status could feel slow with several peers.
  Parallelizing or caching is a behavior change worth its own ticket.
- Snapshot schema test pins only the FIRST array element's shape, so
  `conflicts.entries[0].quarantined_as: string | null` is unpinned when
  the first entry has a value. A null-entry fixture case would tighten it.

