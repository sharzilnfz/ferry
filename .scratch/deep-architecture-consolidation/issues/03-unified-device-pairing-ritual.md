# 03: Unified Device Pairing Ritual

**What to build:** Consolidate `ferry-folder::pairing`, `ferry-crypto::pairing_code`, and `ferry-sync::pairing_transport` into one `PairingRitual` engine that exposes `create_offer(folder)` (6-character short code AND sealed payload envelope) and `accept_offer(code_or_payload)` (accepts either form); transport selection is internal and frontends never branch on it.

**Blocked by:** 01-deep-folder-inventory-module.md

**Status:** in-review

- [x] Define the `PairingRitual` interface: `create_offer` / `accept_offer` / `poll_offer` plus the `PendingOffer` / `PendingAcceptance` / `Accepted` result handles.
- [x] Move rendezvous session state (codes, expiry, one-time consumption) inside the ritual behind a shared in-process map + one-time cross-process rendezvous file.
- [x] Frame the QR payload and the `.ferry-pair` file body as ONE `FERRY1:` envelope so there is no second framing layer to drift.
- [x] Keep the file-transport handshake (offer/response/grant beside the payload, grant sealed under the offer's one-time secret) on the unchanged wire format.
- [x] Delete `crates/ferry-sync/src/pairing_transport.rs` and its tests; rewire `ferry-cli` (share/join/pair), `ferry-daemon` (ipc, ui backend/server), `ferry-gui`, and `ferry-ipc` backend to the ritual.
- [x] Integration tests in `crates/ferry-folder/tests/ritual.rs` cover BOTH rendezvous-style and file-payload acceptance through the single `accept_offer` interface, plus expiry, one-time use, timeout, and already-initialized refusals.

Implementation notes (T-03 landing):

- `crates/ferry-folder/src/pairing.rs` is now the single engine: `PairingRitual::new(home, identity)` joins the process-wide rendezvous (`shared_rendezvous()`); `with_shared` injects an isolated map for tests.
- `accept_offer` detection order: `FERRY1:` envelope string -> in-band answer when the session is reachable (else `no-answer-channel` with guidance to pass the file path); 6-char code -> rendezvous; anything else -> path to a payload file, answered beside itself.
- `create_offer_for_folder(folder_id_hex)` is the daemon IPC entry point (resolves through `FolderInventory` or a registered path override) and opens the folder first.
- Frontends render `PendingOffer::short_code` / `qr_payload()` and `PendingAcceptance::expected_short_code` / `response_path` only; `complete(timeout_secs)` finishes either transport.
- Review fix (03): `ferry-crypto::pairing_code` is deleted; the code mechanics (generate, checksum, constant-time verify, 24h expiry, zeroize) live PRIVATELY inside `ferry-folder::pairing` per ADR-0006 (as amended), so the ritual is the only way to mint or answer a code. Old unit vectors ported to `crates/ferry-folder/tests/pairing_code_tests.rs`.
