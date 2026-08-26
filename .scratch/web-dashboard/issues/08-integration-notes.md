# Ticket 08 — integration notes for wave 2 (ferry-daemon migration)

What wave 1 landed: `crates/ferry-folder` owns folder bootstrap + the
pairing ritual. `ferry-daemon/src/ui/actions.rs` still has its private copy;
migrate it onto these signatures in a later wave. Do NOT re-read
`ferry-cli`'s pairing/folder modules — they are now thin presentation
adapters over this API.

## Imports

```rust
use ferry_folder::folder::{open_folder, OpenFolder};
use ferry_folder::pairing::{
    initiate_begin, initiate_complete, accept_begin, accept_complete,
    Accepted, PairingCompleted, PendingAcceptance, PendingOffer,
};
```

## Open

```rust
pub fn open_folder(root: &Path, identity: &DeviceIdentity)
    -> ferry_folder::FolderResult<OpenFolder>
// OpenFolder { root: PathBuf, settings: Settings, folder_id: [u8; 16],
//              poly: u64, store: Arc<Store> }
//
// NOTE the identity is a parameter — ferry-folder never reads FERRY_HOME.
// Pass st.identity() (or whatever the daemon holds after ticket 12 lands).
// Error codes match today's daemon copy: not-a-folder, config-corrupt,
// not-shared-with-device, key-unwrap, settings-corrupt, settings-version,
// poly-missing.
```

## Share / pair-initiate (replaces `actions::share`)

```rust
pub fn initiate_begin(opened: &OpenFolder, identity: &DeviceIdentity)
    -> FolderResult<PendingOffer>;
// PendingOffer { offer_bytes: Vec<u8>,      // QR content == file bytes
//                short_code: String,
//                offer_path: PathBuf }      // where complete() will write

pub fn initiate_complete(pending: PendingOffer, opened: &OpenFolder,
                         identity: &DeviceIdentity, timeout_secs: u64)
    -> FolderResult<PairingCompleted>;
// PairingCompleted { peer_device_id: DeviceId ([u8;32]), folder_id,
//                    short_code, offer_path, grant_path }
//
// Writes pair-offer.ferry-pair, polls pair-response.ferry-pair (200ms tick),
// completes the transcript MAC, appends the peer wrap to CONFIG_HEAD,
// seals+writes pair-grant.ferry-grant. Timeout => code "pair-timeout".
```

Call `initiate_begin`, render/return `short_code` (+ optional QR from
`offer_bytes`) to the browser, then run `initiate_complete`. The current
daemon's synchronous poll maps 1:1: begin, build the JSON minus status, then
complete and fill it in. The secret-scan gate is a CALLER contract: link
`ferry-ignore` and refuse with `secrets-found` before calling
`initiate_begin` (see `ferry-cli/src/commands/share.rs` for the exact shape).

## Pair-accept (replaces `actions::pair_accept`)

```rust
pub fn accept_begin(identity: &DeviceIdentity, offer_file: &Path,
                    dir: Option<&Path>)
    -> FolderResult<PendingAcceptance>;
// PendingAcceptance {
//     expected_short_code: String,   // human comparison only
//     response_path: PathBuf,        // our response, already written
//     grant_path: PathBuf,           // where the grant must appear
//     .. }                            // opaque payload fields (pub(crate))
//
// Refuses an initialized target with code "already-initialized"; writes
// pair-response.ferry-pair here (offer must be on a writable shared path).

pub fn accept_complete(pending: PendingAcceptance, identity: &DeviceIdentity,
                       timeout_secs: u64)
    -> FolderResult<Accepted>;
// Accepted { folder: PathBuf, folder_id: [u8;16] }
//
// Polls the grant (=> "pair-timeout"), adopts store + settings, records
// BOTH devices in the acceptor's CONFIG_HEAD. Timeout => "pair-timeout".
```

## Error type

```rust
pub struct FolderError { pub code: &'static str,   // v0-frozen codes
                         pub message: String,
                         pub hint: String }
pub type FolderResult<T> = Result<T, FolderError>; // Display impl included
```
Map onto `OpError::new(e.code, e.message, e.hint)` at the HTTP boundary.

## Also available (bootstrap)

`create_folder(root, identity, folder_id, poly) -> FolderResult<(Store, Fmk)>`,
`adopt_folder(...) -> FolderResult<Store>`,
`find_polynomial(&Store) -> FolderResult<u64>`,
`Settings` / `save_settings` / `load_rules` / `state_dir` / `dot_dir` /
`short_device` — all under `ferry_folder::folder::`.

## Migration notes

- `actions.rs`'s `open_folder` copy can be deleted wholesale once `/api/share`
  and `/api/pair/accept` call these; its reduced error set is subsumed.
- `folder_poly(st)` becomes `opened.poly` (no zero-key Store reopen).
- Artifact filename constants (`OFFER_FILE` etc.) come from
  `ferry_folder::pairing::{OFFER_SUFFIX, RESPONSE_SUFFIX, GRANT_SUFFIX}`.
- The daemon's missing secret-scan gate is the drift ticket 08 exists to
  kill: wire `ferry-ignore` into `/api/share` during the same migration.
