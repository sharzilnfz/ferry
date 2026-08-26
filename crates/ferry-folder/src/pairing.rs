//! The pairing ritual: write offer, poll for response, complete the
//! transcript MAC, wrap the FMK for the peer, append the wrap entry to
//! `CONFIG_HEAD`, seal the grant. One implementation for every frontend.
//!
//! # File transport (documented honestly)
//!
//! A real phone-camera QR scan moves ~93 bytes out-of-band. Across machines
//! there is no camera, so v0 uses PAYLOAD FILES standing in for that
//! channel:
//!
//! ```text
//! device A (in the folder to share)
//!   -> initiate_begin    builds the offer (bytes + short code)
//!      [frontend renders QR / code / instructions here]
//!   -> initiate_complete writes <folder>/.ferry/pair-offer.ferry-pair,
//!                        polls for pair-response.ferry-pair, completes the
//!                        MAC, appends the peer wrap, seals pair-grant.ferry-grant
//!
//! device B (in the folder to adopt)
//!   -> accept_begin      parses the offer, writes pair-response.ferry-pair
//!      [frontend shows the response path + expected short code here]
//!   -> accept_complete   polls for the grant, adopts store + settings,
//!                        records both devices in its CONFIG_HEAD
//! ```
//!
//! Moving the two files between machines is the user's out-of-band act
//! (AirDrop/scp/USB) — exactly the trust step a camera scan performs.
//! Possession of the offer authorizes pairing; the FMK is wrapped only
//! AFTER the response MAC proves both sides saw the same transcript.
//!
//! The grant file is sealed under a key derived from the offer's one-time
//! secret (HKDF-SHA-256, behind `ferry-crypto`), so only an acceptor holding
//! those exact bytes can open it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ferry_crypto::folder_key::{unwrap_folder_key, wrap_folder_key, WRAPPED_LEN};
use ferry_crypto::identity::{DeviceId, DeviceIdentity};
use ferry_crypto::pairing::{
    complete_pairing, open_pair_grant, respond, seal_pair_grant, GrantError, PairingOffer,
    TransportHints,
};
use serde_json::json;

use crate::error::{CodeInto, FolderError, FolderResult};
use crate::folder::{
    adopt_folder, append_wrap_entry_for, dot_dir, save_settings, unwrap_own_fmk, OpenFolder,
    Settings, SETTINGS_FORMAT_VERSION,
};

pub const OFFER_SUFFIX: &str = "pair-offer.ferry-pair";
pub const RESPONSE_SUFFIX: &str = "pair-response.ferry-pair";
pub const GRANT_SUFFIX: &str = "pair-grant.ferry-grant";

/// Where pairing artifacts live for one folder.
fn artifact(folder_dot: &Path, suffix: &str) -> PathBuf {
    folder_dot.join(suffix)
}

// ---------------------------------------------------------------------------
// initiate (device A)
// ---------------------------------------------------------------------------

/// The initiator's half of the ritual, ready for the frontend to render:
/// QR content bytes, the human short code, and where the offer file will
/// land. Nothing is written to disk yet — render first, then call
/// [`initiate_complete`], which creates the file watchers look for.
pub struct PendingOffer {
    /// Bytes to encode in a QR symbol (identical to the offer file bytes).
    pub offer_bytes: Vec<u8>,
    /// Human-typed confirmation code (compare across devices).
    pub short_code: String,
    /// Where [`initiate_complete`] writes the offer file.
    pub offer_path: PathBuf,
}

/// Everything that happened when the initiator's side finished.
pub struct PairingCompleted {
    /// The device we just paired with (X25519 public key).
    pub peer_device_id: DeviceId,
    pub folder_id: [u8; 16],
    /// Short code for this offer (same as [`PendingOffer::short_code`]).
    pub short_code: String,
    pub offer_path: PathBuf,
    /// Where the sealed grant was written for the acceptor.
    pub grant_path: PathBuf,
}

/// Build the initiator's offer inside an opened folder. Validates the folder
/// is actually openable by this device (FMK unwraps) BEFORE any artifact is
/// rendered or written.
pub fn initiate_begin(
    opened: &OpenFolder,
    identity: &DeviceIdentity,
) -> FolderResult<PendingOffer> {
    let dot = dot_dir(&opened.root);
    std::fs::create_dir_all(&dot).code("io", "check folder permissions")?;
    let _fmk = unwrap_own_fmk(opened, identity)?;

    let now = ferry_sync_engine::timefmt::now_unix();
    let offer = PairingOffer::create(opened.folder_id, identity, now.0);
    let offer_bytes = offer.serialize();
    let short_code = offer.short_code(TransportHints(0));
    Ok(PendingOffer {
        offer_bytes,
        short_code,
        offer_path: artifact(&dot, OFFER_SUFFIX),
    })
}

/// Write the offer file, poll for the responder's response beside it, and
/// finish the ritual: transcript MAC, wrap entry in OUR `CONFIG_HEAD`,
/// sealed grant for the acceptor. Polls up to `timeout_secs`, then fails
/// with `pair-timeout`.
pub fn initiate_complete(
    pending: PendingOffer,
    opened: &OpenFolder,
    identity: &DeviceIdentity,
    timeout_secs: u64,
) -> FolderResult<PairingCompleted> {
    let fmk = unwrap_own_fmk(opened, identity)?;
    let offer =
        PairingOffer::parse(&pending.offer_bytes).map_err(|e| {
            FolderError::new(
                "bad-offer",
                e.to_string(),
                "internal inconsistency; retry the pairing",
            )
        })?;

    std::fs::write(&pending.offer_path, &pending.offer_bytes)
        .code("io", format!("cannot write {}", pending.offer_path.display()))?;

    // Waiting phase: poll for the response file beside OUR offer.
    let response_path = pending.offer_path.with_file_name(RESPONSE_SUFFIX);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let response_bytes = loop {
        match std::fs::read(&response_path) {
            Ok(bytes) => break bytes,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => {
                return Err(FolderError::new(
                    "pair-timeout",
                    format!(
                        "no response appeared within {timeout_secs}s at {}",
                        response_path.display()
                    ),
                    "run the accepting step against this offer file on the other device, then retry",
                ))
            }
        }
    };

    let response = ferry_crypto::pairing::PairingResponse::parse(&response_bytes).code(
        "pair-bad-response",
        "the response file is damaged; have the other device re-run accept",
    )?;
    let done = complete_pairing(&offer, &pending.offer_bytes, &response, &fmk, identity).map_err(
        |e| {
            FolderError::new(
                "pair-verify",
                e.to_string(),
                "the response did not match this offer; start over with a fresh offer",
            )
        },
    )?;

    // Grant access: append the peer's wrap to OUR config head so the folder
    // records every authorized device.
    append_wrap_entry_for(&opened.root, opened.folder_id, &done.peer_pub, &done.wrapped_for_peer)?;

    // Sealed handoff for the acceptor: folder key wrap + chunker polynomial.
    let grant = seal_grant(&pending.offer_bytes, &done.wrapped_for_peer, opened.poly)?;
    let grant_path = artifact(&dot_dir(&opened.root), GRANT_SUFFIX);
    std::fs::write(&grant_path, &grant)
        .code("io", format!("cannot write {}", grant_path.display()))?;

    Ok(PairingCompleted {
        peer_device_id: done.peer_pub,
        folder_id: opened.folder_id,
        short_code: pending.short_code,
        offer_path: pending.offer_path,
        grant_path,
    })
}

// ---------------------------------------------------------------------------
// accept (device B)
// ---------------------------------------------------------------------------

/// The acceptor's half of the ritual after [`accept_begin`]: what to show
/// the human plus what [`accept_complete`] needs to finish.
pub struct PendingAcceptance {
    /// Full offer payload (drives grant unsealing in `accept_complete`).
    pub(crate) offer_bytes: Vec<u8>,
    /// Directory receiving the adopted folder.
    pub(crate) target: PathBuf,
    /// Short code the human should compare against the other screen.
    pub expected_short_code: String,
    /// Where OUR response was written for the initiator to pick up.
    pub response_path: PathBuf,
    /// Where the initiator's sealed grant must appear.
    pub grant_path: PathBuf,
}

/// What the acceptor ended up with.
pub struct Accepted {
    /// The adopted folder root (the `dir` given to [`accept_begin`], or `.`
    /// when none was).
    pub folder: PathBuf,
    pub folder_id: [u8; 16],
}

/// Parse an offer file and answer it. Refuses an already-initialized target;
/// writes the response file where the initiator looks for it (so the offer
/// must live on a writable shared location). Render the returned paths and
/// expected short code, then call [`accept_complete`].
pub fn accept_begin(
    identity: &DeviceIdentity,
    offer_file: &Path,
    dir: Option<&Path>,
) -> FolderResult<PendingAcceptance> {
    let offer_bytes = std::fs::read(offer_file).code(
        "not-found",
        "check the path to the offer file printed by the sharing device",
    )?;
    let offer = PairingOffer::parse(&offer_bytes).map_err(|e| {
        FolderError::new(
            "bad-offer",
            e.to_string(),
            "get a fresh offer file from the sharing device",
        )
    })?;

    let target = dir.unwrap_or(Path::new("."));
    if dot_dir(target).is_dir() {
        return Err(FolderError::new(
            "already-initialized",
            format!("{} already contains a .ferry store", target.display()),
            "cd into the empty directory you want synced, or remove the old store deliberately",
        ));
    }

    // Informational over file transport, but both humans compare screens.
    let expected_short_code = offer.short_code(TransportHints(0));

    // Our half of the ritual, written where the initiator looks for it.
    let response = respond(&offer, identity, ferry_sync_engine::timefmt::now_unix().0);
    let response_path = offer_file.with_file_name(RESPONSE_SUFFIX);
    std::fs::write(&response_path, response.serialize()).code(
        "io",
        format!(
            "cannot write {} (offer must live on a writable shared location)",
            response_path.display()
        ),
    )?;

    Ok(PendingAcceptance {
        offer_bytes,
        target: target.to_path_buf(),
        expected_short_code,
        response_path,
        grant_path: offer_file.with_file_name(GRANT_SUFFIX),
    })
}

/// Poll for the sealed grant, then adopt the folder: unwrap the FMK, build
/// the local store, persist settings, record BOTH devices in our
/// `CONFIG_HEAD`. Polls up to `timeout_secs`, then fails with `pair-timeout`.
pub fn accept_complete(
    pending: PendingAcceptance,
    identity: &DeviceIdentity,
    timeout_secs: u64,
) -> FolderResult<Accepted> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let grant_bytes = loop {
        match std::fs::read(&pending.grant_path) {
            Ok(bytes) => break bytes,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => {
                return Err(FolderError::new(
                    "pair-timeout",
                    format!(
                        "no grant appeared within {timeout_secs}s at {}",
                        pending.grant_path.display()
                    ),
                    "make sure the other device completed its initiating step and copied pair-grant.ferry-grant beside the offer file",
                ))
            }
        }
    };

    let (folder_id, poly, wrapped_for_peer) = open_grant(&pending.offer_bytes, &grant_bytes)?;
    let fmk =
        *unwrap_folder_key(&wrapped_for_peer, &folder_id, identity).map_err(|e| {
            FolderError::new(
                "key-unwrap",
                e.to_string(),
                "the grant did not address this device; re-run the pairing",
            )
        })?;

    // Build the local store around the adopted key material.
    let store = adopt_folder(&pending.target, identity, folder_id, &fmk, poly)?;
    store
        .flush()
        .map_err(|e| FolderError::new("store", e.to_string(), "retry"))?;
    store
        .write_index_snapshot()
        .map_err(|e| FolderError::new("store", e.to_string(), "retry"))?;
    let settings = Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: ferry_store::format::hex(&folder_id),
        honor_gitignore: false,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    save_settings(&pending.target, &settings)?;

    // The acceptor's CONFIG_HEAD must name EVERY authorized device, not just
    // itself: the engine seeds its peer allow-list from CONFIG_HEAD wrap
    // entries and denies unknown peers. `adopt_folder` wrote a single-entry
    // head {us}; without the initiator's entry the first session to the
    // owner is rejected as unauthorized and never converges. Re-wrap the FMK
    // for the initiator so the appended record is a real, usable key
    // envelope — mirroring what `append_wrap_entry_for` does on the
    // initiator's side.
    let initiator = PairingOffer::parse(&pending.offer_bytes)
        .map_err(|e| {
            FolderError::new(
                "bad-offer",
                e.to_string(),
                "internal inconsistency; redo the pairing",
            )
        })?
        .initiator_pub;
    let wrapped_for_initiator = wrap_folder_key(&fmk, &folder_id, &initiator).code(
        "crypto",
        "identity keys are local; retry with a fresh identity if this repeats",
    )?;
    append_wrap_entry_for(&pending.target, folder_id, &initiator, &wrapped_for_initiator)?;

    Ok(Accepted {
        folder: pending.target,
        folder_id,
    })
}

// ---------------------------------------------------------------------------
// grant sealing (A -> B handoff)
// ---------------------------------------------------------------------------

/// Key derivation and AEAD sealing live behind ferry-crypto: this module
/// only builds the JSON body and maps errors.
fn seal_grant(
    offer_bytes: &[u8],
    wrapped_for_peer: &[u8; WRAPPED_LEN],
    poly: u64,
) -> FolderResult<Vec<u8>> {
    let body = json!({
        "wrapped_for_peer": hex_of(wrapped_for_peer),
        "poly": poly,
    })
    .to_string()
    .into_bytes();
    seal_pair_grant(offer_bytes, &body).map_err(grant_error)
}

fn open_grant(offer_bytes: &[u8], raw: &[u8]) -> FolderResult<([u8; 16], u64, [u8; WRAPPED_LEN])> {
    let body = open_pair_grant(offer_bytes, raw).map_err(grant_error)?;
    let doc: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| FolderError::new("bad-grant", "grant body unreadable", "redo the pairing"))?;
    let wrapped_hex = doc["wrapped_for_peer"]
        .as_str()
        .ok_or_else(|| FolderError::new("bad-grant", "grant body incomplete", "redo the pairing"))?;
    let poly = doc["poly"]
        .as_u64()
        .ok_or_else(|| FolderError::new("bad-grant", "grant body incomplete", "redo the pairing"))?;
    let wrapped = unhex_80(wrapped_hex)?;
    // folder_id rides in the offer itself (offsets pinned by ferry-crypto's
    // v1 offer layout); bounds-checked, no panic path.
    let folder_id: [u8; 16] = offer_bytes
        .get(5..21)
        .and_then(|s| <&[u8] as TryInto<[u8; 16]>>::try_into(s).ok())
        .ok_or_else(|| {
            FolderError::new(
                "bad-grant",
                "the offer file is truncated",
                "get a fresh offer file from the sharing device",
            )
        })?;
    Ok((folder_id, poly, wrapped))
}

/// Map ferry-crypto's grant errors onto targeted coded errors.
fn grant_error(e: GrantError) -> FolderError {
    match e {
        GrantError::Malformed { .. } => FolderError::new(
            "bad-grant",
            "the grant file is malformed",
            "have the other device re-run its initiating step",
        ),
        GrantError::Auth => FolderError::new(
            "bad-grant",
            "the grant file failed authentication",
            "it must travel together with THIS exact offer file; redo the pairing",
        ),
        GrantError::OfferTruncated { .. } => FolderError::new(
            "bad-grant",
            "the offer file is truncated",
            "get a fresh offer file from the sharing device",
        ),
        GrantError::Internal => FolderError::new("crypto", "grant seal failed", "retry"),
    }
}

fn unhex_80(s: &str) -> FolderResult<[u8; WRAPPED_LEN]> {
    ferry_store::format::unhex::<WRAPPED_LEN>(s).ok_or_else(|| {
        FolderError::new(
            "bad-grant",
            "grant key envelope is not 160 hex chars",
            "redo the pairing",
        )
    })
}

fn hex_of(b: &[u8]) -> String {
    ferry_store::format::hex(b)
}
