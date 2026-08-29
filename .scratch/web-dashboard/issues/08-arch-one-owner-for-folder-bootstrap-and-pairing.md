# One owner for folder bootstrap and the pairing ritual

Status: done
Depends on:
Blocks:

## Files

- `crates/ferry-cli/src/folder.rs` (`open_folder`, `OpenFolder`, `create_folder`,
  `adopt_folder`, `find_polynomial`)
- `crates/ferry-cli/src/commands/pairing.rs` (the offer/response/grant ritual,
  501 lines)
- `crates/ferry-cli/src/commands/share.rs` (secret-scan gate on top of the ritual;
  NOTE mid-edit by a concurrent fix agent — read around it)
- `crates/ferry-daemon/src/ui/actions.rs` (`open_folder`, `append_wrap_entry`,
  `share`, `pair_accept`, `poll_for_file` — a second, smaller copy of all of the
  above)

## Problem

The pairing ritual — write offer, poll for response, complete transcript MAC,
wrap FMK for peer, append wrap entry to `CONFIG_HEAD`, seal grant — exists twice.
`ferry-daemon/src/ui/actions.rs` re-implements it because "the daemon binary
cannot depend on ferry-cli" (true: a bin crate can't be a dependency), and then
keeps going with the same reasoning where it does NOT hold:

- Its `open_folder` is a reduced copy of `ferry-cli/folder.rs::open_folder`
  (same CONFIG_HEAD parse, same unwrap, same not-shared error codes).
- The secret-scan gate from `ferry share` is silently absent ("the secret
  scanner lives behind ferry-ignore, which the daemon binary cannot depend on
  yet") — false: `ferry-ignore` is a leaf library, no cycle exists. The
  `/api/share` endpoint therefore shares folders without ever flagging `.env`.
- The poll loops, artifact filename constants, and wrap-entry append are
  copy-paste variants of `pairing.rs`.

The interface is nearly as complex as the implementation (a shallow module
split across two crates): both copies must know every artifact name, every
error code, and the FMK-wrap ordering. A protocol change to pairing must land
in two places or the frontends drift — and they already have, in the one place
that guards secrets.

## Solution

Extract a `ferry-folder` crate (workspace convention favors small crates):
folder bootstrap (`open` / `create` / `adopt`, settings, polynomial lookup) plus
the payload-file ritual (`initiate` / `accept`) returning plain structs —
no QR art, no CLI output shaping, no JSON documents. Both frontends become thin
adapters: `ferry-cli` adds the secret-scan gate and rendering; the daemon UI maps
results to `{command, status, ...}` documents and links `ferry-ignore` for the
gate it currently skips.

## Benefits

- Locality: a pairing-protocol change lands in exactly one module.
- The interface becomes the test surface: the ritual is exercised once by unit
  tests in `ferry-folder`; CLI integration tests and dashboard e2e both inherit
  the coverage instead of re-testing private copies.
- The secrets-found gate can no longer be skipped by a frontend forgetting to
  wire it — it lives inside `initiate`'s caller contract.
- Deletion test passes: deleting `ferry-folder` would force every frontend to
  re-implement bootstrap + ritual, i.e. complexity concentrates there today.

## Before / after

```text
BEFORE                                  AFTER
ferry-cli/commands/pairing.rs           ferry-folder/
ferry-cli/folder.rs                       open/create/adopt
ferry-daemon/ui/actions.rs   (copy)       initiate/accept ritual
                                        ferry-cli        ferry-daemon/ui
                                          = thin adapters over ferry-folder
```

## Strength

Strong

## Comments

Full analysis with diagrams: /var/folders/y9/hnkm2lv91n5chc4116wp_hf40000gn/T/architecture-review-1787745437.html (architecture audit A0, 2026-08-26).

### Wave 1 report — CLI side + new crate (2026-08-26)

**What moved where**

- NEW `crates/ferry-folder` (`src/folder.rs`, `src/pairing.rs`, `src/error.rs`,
  tests `bootstrap.rs`, `ritual.rs`):
  - from `ferry-cli/src/folder.rs`: `Settings`, `SETTINGS_FORMAT_VERSION`,
    `CONFIG_FILE`/`SETTINGS_FILE`/`DOT_DIR`, `dot_dir`/`state_dir`,
    `load_rules`, `create_folder`, `adopt_folder`, `OpenFolder`,
    `find_polynomial`, `DEFAULT_FERRY_IGNORE`, `write_default_ignore_if_absent`,
    `save_settings`, `short_device`. Internal helpers `unwrap_own_fmk` +
    `append_wrap_entry_for` moved too.
  - from `ferry-cli/src/commands/pairing.rs`: the whole ritual core — offer
    create/serialize/parse, response poll, transcript-MAC completion,
    wrap-entry append, grant seal/open, artifact suffixes
    (`pair-offer.ferry-pair`, `pair-response.ferry-pair`,
    `pair-grant.ferry-grant`). Error codes unchanged (v0-frozen).
  - error type: `FolderError { code: &'static str, message, hint }` — no
    serde_json detail field (that is CLI presentation), codes byte-identical.
- STAYS in `ferry-cli/src/commands/pairing.rs`: QR art (`render_ascii_qr`),
  stderr instructions, canonical path display, and the
  `{command:"pair",...}` Output documents. Byte-for-byte identical human text,
  JSON keys, and print ordering (QR prints before the offer file exists).
- STAYS in `ferry-cli/src/commands/share.rs`: the secrets-found gate,
  untouched (preserves the concurrent agent's warnings-shape fix;
  share_gating tests pass unmodified).
- `ferry-cli/src/folder.rs` is now a ~75-line adapter: re-exports + thin
  wrappers converting `FolderError` → `CliError`; its `open_folder(root)`
  resolves identity via FERRY_HOME then delegates to
  `ferry_folder::folder::open_folder(root, &identity)`.
- Root `Cargo.toml`: `ferry-folder = { path = "crates/ferry-folder" }` in
  `[workspace.dependencies]`; `ferry-cli/Cargo.toml` consumes it via
  `ferry-folder.workspace = true`. Members glob picked up the crate; no new
  external deps.

**API surface for non-CLI callers** (no clap, no FERRY_HOME inside
ferry-folder): two-phase ritual so any frontend can render between steps —

```text
initiate_begin(&OpenFolder, &DeviceIdentity) -> PendingOffer   // nothing on disk yet
initiate_complete(PendingOffer, &OpenFolder, &DeviceIdentity, timeout) -> PairingCompleted
accept_begin(&DeviceIdentity, offer_file, dir: Option<&Path>) -> PendingAcceptance // writes response
accept_complete(PendingAcceptance, &DeviceIdentity, timeout) -> Accepted
open_folder(root, &identity) -> OpenFolder   // identity is a parameter
```

Exact signatures + struct fields for wave 2:
`.scratch/web-dashboard/issues/08-integration-notes.md`.

**Tests**

- Ported/direct coverage in `crates/ferry-folder/tests/`: bootstrap round-trip
  (create→open→poly lookup), not-a-folder / not-shared-with-device /
  already-initialized codes, adopt layout + single-wrap head, settings
  format-version stability, default-ignore write-once; ritual loopback with
  two in-process identities (both CONFIG_HEADs end with both devices, frozen
  artifact names asserted), accept-refuses-initialized-target,
  initiate-timeout, accept-timeout. 11 tests.
- All existing ferry-cli tests pass UNMODIFIED (commands, json_schema,
  pin_cli, cli_parse, share_gating incl. the secrets gate shape, and the
  exchange_loopback e2e through the real binary).

**Verification**

- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: 61 suites, all ok, 0 failures (run twice; second
  run clean — no concurrent-agent flakes at this time). Targeted `-p
  ferry-folder` and `-p ferry-cli` runs green.

**Not done here (wave 2)**: migrating `ferry-daemon/src/ui/actions.rs` onto
this API and wiring `ferry-ignore` into `/api/share`'s missing secret-scan
gate — see integration notes above.

### Wave 2 report — daemon migration (2026-08-26)

**What changed**

- `crates/ferry-daemon/Cargo.toml`: added `ferry-folder.workspace = true` and `ferry-ignore = { path = "../ferry-ignore" }` (leaf crate, no cycle; `cargo tree -p ferry-daemon` confirms ferry-folder -> ferry-ignore chain, no new external deps).
- `crates/ferry-daemon/src/ui/actions.rs`: deleted private `open_folder` copy (CONFIG_HEAD parse + unwrap), `append_wrap_entry`, `poll_for_file`, constants `OFFER_FILE/RESPONSE_FILE/GRANT_FILE`. Now calls `ferry_folder::folder::open_folder(root, &identity)` where identity is `st.identity()` (UiState already holds DeviceIdentity via `main.rs:load_or_create` at `store/.device-identity`, no new state needed). `share` and `pair_accept` replaced with `initiate_begin`/`initiate_complete` and `accept_begin`/`accept_complete` from `ferry_folder::pairing`. `folder_poly(st)` removed, now `opened.poly`. Error mapping via `folder_err` helper preserves v0-frozen codes; `PAIR_TIMEOUT_SECS=120` unchanged, 200/409 mapping identical.
- `crates/ferry-daemon/src/ui/mod.rs`: extended `OpError` with `detail: Option<Value>` and `with_detail` builder; `From<OpError> for ApiError` merges object detail into body so secrets-found warnings appear as top-level `warnings` array (409). `api_share` now threads `i_know` bool from JSON into `actions::share(st, folder, i_know)` instead of ignoring it.
- Secret-scan gate in `actions::share`: before `initiate_begin`, calls `ferry_folder::folder::load_rules` + `ferry_ignore::secrets::scan_for_secrets`, builds `warnings: Vec<Value>` with shape `{path, line, class, preview}` per `docs/cli-json.md` (copied from `ferry-cli/src/commands/share.rs` lines 28-74). If non-empty and `!i_know`, returns `OpError::new("secrets-found", …, "review each path: exclude it (`ferry ignore '<pattern>'`) or accept the risk with --i-know").with_detail(json!({"warnings": warnings}))` → 409 with warnings array, byte-identical to CLI gate. `warnings_reviewed` and `warnings` fields in success document preserved.

**Preserved**

- `actions.rs` pin/start/stop/release, `pin_store`/`pin_err`/`held_by_peer`/`conflict_entries` unchanged (ticket 09's `PinStore/HeldLedger/list_conflicts` ownership retained).
- HTTP surface byte-identical: same JSON keys, same error codes (`pair-timeout`, `already-initialized`, `not-shared-with-device`, `secrets-found`, etc.), same 409 for secrets-found/pair-timeout. Wire format (payload file layout) v0-frozen unchanged as it lives in ferry-folder.

**Verification**

- `cargo clippy -p ferry-daemon --all-targets -- -D warnings`: clean (fixed format_push_string lints via `writeln!`).
- `cargo test -p ferry-daemon`: 2 passed (deferred_and_gate_codes_map_to_their_spec_statuses, assets_embed_with_mime_types).
- `cargo test -p ferry-folder -p ferry-cli -p ferry-ignore -p ferry-daemon`: all suites green (40+ ignored policy tests, share_gating 2/2, etc.).
- `cargo test --workspace`: failures only in `ferry-sync` (engine, peer_policy, pin_enforcement) due to `device_identity_for_tag` gated behind `#[cfg(test)]` and missing `Ctx` fields — owned by R-ENGINE, not this wave; all daemon/folder/cli/ignore crates green.
- `cargo tree` no cycle: ferry-daemon -> ferry-folder -> ferry-ignore leaf.
- E2E TCP pair with --ui (127.0.0.1:18xxx, separate store/tree dirs, identities copied from FERRY_HOME):
  - `POST /api/share` with `.env` containing `AKIA…` and `!.env` opt-in and `i_know:false` → `409 {"code":"secrets-found","warnings":[{"class":"env-file-included",…},{"class":"aws-access-key","line":1,…}]}` correct.
  - `POST /api/share` with `i_know:true` → creates `pair-offer.ferry-pair` (93 bytes) and polls; `POST /api/pair/accept` with payload_path → `200 {"command":"pair","role":"accept","expected_short_code":"2224-…"}`, initiator share then completes `200 {"command":"share","peer_device_id":…,"short_code":"2224-…","warnings":[]}`; both `.ferry/config` files grow to 266 bytes (2 wrap entries), artifact names `pair-offer.ferry-pair`/`pair-response.ferry-pair`/`pair-grant.ferry-grant` frozen.
  - `POST /api/pin/start` / `stop` still works (200, `base_peers_recorded`, `held_by_peer`).

**Cut / deferred**

- Nothing deferred; secret-scan gate landed. No new external deps, no comments unless essential, clippy clean.
