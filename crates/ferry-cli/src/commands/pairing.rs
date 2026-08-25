//! The pairing ritual as a CLI flow (ferry-crypto does the crypto; this
//! module does files, codes, QR art, and gating).
//!
//! # File transport (documented honestly)
//!
//! A real phone-camera QR scan moves ~93 bytes out-of-band. Across machines
//! there is no camera, so v0 uses PAYLOAD FILES standing in for that
//! channel:
//!
//! ```text
//! device A: ferry pair            (in the folder to share)
//!   -> writes  <folder>/.ferry/pair-offer.ferry-pair    (the "QR bytes")
//!   -> prints  short code + ASCII QR + the file path
//!   -> waits for pair-response.ferry-pair next to its offer file
//!
//! device B: ferry pair --accept <offer-file>   (in the folder to adopt)
//!   -> verifies code format, writes pair-response.ferry-pair beside the offer
//!   -> waits for pair-grant.ferry-grant (sealed folder key + polynomial)
//!   -> creates its local store and unwraps the FMK
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

use ferry_crypto::folder_key::{unwrap_folder_key, Fmk, WRAPPED_LEN};
use ferry_crypto::identity::DeviceIdentity;
use ferry_crypto::pairing::{
    complete_pairing, open_pair_grant, respond, seal_pair_grant, GrantError, PairingOffer,
    TransportHints,
};
use serde_json::json;

use crate::error::{CliError, CliResult, CodeInto};
use crate::folder::{self, OpenFolder, Settings, SETTINGS_FORMAT_VERSION};
use crate::out::Output;

use self::folder::save_settings;

pub const OFFER_SUFFIX: &str = "pair-offer.ferry-pair";
pub const RESPONSE_SUFFIX: &str = "pair-response.ferry-pair";
pub const GRANT_SUFFIX: &str = "pair-grant.ferry-grant";

/// Where pairing artifacts live for one folder.
fn artifact(folder_dot: &Path, suffix: &str) -> PathBuf {
    folder_dot.join(suffix)
}

/// Canonical display for an artifact path (falls back to raw on error).
fn offer_path_display(p: &Path) -> std::path::Display<'_> {
    let _ = p.canonicalize();
    p.display()
}

// ---------------------------------------------------------------------------
// initiate (device A)
// ---------------------------------------------------------------------------

/// Run the initiator side inside an opened folder. Polls for the responder's
/// response + completes the FMK wrap when it appears.
pub fn initiate(
    opened: &OpenFolder,
    identity: &DeviceIdentity,
    timeout_secs: u64,
) -> CliResult<Output> {
    let dot = folder::dot_dir(&opened.root);
    std::fs::create_dir_all(&dot).code("io", "check folder permissions")?;
    let fmk = unwrap_own_fmk(opened, identity)?;

    let now = ferry_sync_engine::timefmt::now_unix();
    let offer = PairingOffer::create(opened.folder_id, identity, now.0);
    let offer_bytes = offer.serialize();

    // Human artifacts FIRST (code + QR + instructions), so nothing can race
    // a reader that watches for the offer file.
    let offer_path = artifact(&dot, OFFER_SUFFIX);
    let code = offer.short_code(TransportHints(0));
    let qr = render_ascii_qr(&offer_bytes)?;
    eprintln!("{qr}");
    eprintln!(
        "Short code (compare on the other device): {code}\nOffer file: {}",
        offer_path_display(&offer_path)
    );
    eprintln!(
        "On the other device run:\n  ferry pair --accept {}",
        offer_path_display(&offer_path)
    );

    std::fs::write(&offer_path, &offer_bytes)
        .code("io", format!("cannot write {}", offer_path.display()))?;

    // Waiting phase: poll for the response file beside OUR offer.
    let response_path = offer_path.with_file_name(RESPONSE_SUFFIX);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let response_bytes = loop {
        match std::fs::read(&response_path) {
            Ok(bytes) => break bytes,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => {
                return Err(CliError::new(
                    "pair-timeout",
                    format!(
                        "no response appeared within {timeout_secs}s at {}",
                        response_path.display()
                    ),
                    "run `ferry pair --accept <offer-file>` on the other device, then retry",
                ))
            }
        }
    };

    let response = ferry_crypto::pairing::PairingResponse::parse(&response_bytes).code(
        "pair-bad-response",
        "the response file is damaged; have the other device re-run accept",
    )?;
    let done = complete_pairing(&offer, &offer_bytes, &response, &fmk, identity).map_err(|e| {
        CliError::new(
            "pair-verify",
            e.to_string(),
            "the response did not match this offer; start over with a fresh `ferry pair`",
        )
    })?;

    // Grant access: append the peer's wrap to OUR config head so the folder
    // records every authorized device.
    append_wrap_entry_for(
        &opened.root,
        opened.folder_id,
        &done.peer_pub,
        &done.wrapped_for_peer,
    )?;

    // Sealed handoff for the acceptor: folder key wrap + chunker polynomial.
    let grant = seal_grant(&offer_bytes, &done.wrapped_for_peer, opened.poly)?;
    let grant_path = artifact(&dot, GRANT_SUFFIX);
    std::fs::write(&grant_path, &grant)
        .code("io", format!("cannot write {}", grant_path.display()))?;

    let json_doc = json!({
        "command": "pair",
        "role": "initiate",
        "status": "completed",
        "folder": opened.root.display().to_string(),
        "folder_id": ferry_store::format::hex(&opened.folder_id),
        "peer_device_id": ferry_store::format::hex(&done.peer_pub),
        "short_code": code,
        "offer_file": offer_path.display().to_string(),
    });
    let human = format!(
        "Paired with device {}.\nIts copy of this folder's key is sealed at:\n  {}\nNext on the OTHER device: start syncing with\n  ferry daemon --peer-url <this-device-address>\n(or `ferry sync --peer-url ...` once).",
        folder::short_device(&done.peer_pub),
        grant_path.display()
    );
    Ok(Output::new(json_doc, human))
}

/// Read this folder's `CONFIG_HEAD` entry for our device and unwrap the FMK.
pub fn unwrap_own_fmk(opened: &OpenFolder, identity: &DeviceIdentity) -> CliResult<Fmk> {
    let head_bytes = std::fs::read(folder::dot_dir(&opened.root).join(folder::CONFIG_FILE)).code(
        "config-corrupt",
        "the folder's key envelope is missing or unreadable",
    )?;
    let head = ferry_crypto::config_head::parse_config_head(&head_bytes).code(
        "config-corrupt",
        "restore from backup or re-pair the folder",
    )?;
    let entry = head
        .entries
        .iter()
        .find(|e| e.device_pub == *identity.public())
        .ok_or_else(|| {
            CliError::new(
                "not-shared-with-device",
                "this folder was not shared with this device",
                "run `ferry init` here or ask the owner to share again",
            )
        })?;
    Ok(
        *unwrap_folder_key(&entry.wrapped, &head.folder_id, identity).code(
            "key-unwrap",
            "your device.key may have changed; restore it or re-pair",
        )?,
    )
}

/// Append one wrapped-key entry to the folder's `CONFIG_HEAD` (idempotent per
/// recipient device).
fn append_wrap_entry_for(
    root: &Path,
    folder_id: [u8; 16],
    recipient: &ferry_crypto::identity::DeviceId,
    wrapped: &[u8; WRAPPED_LEN],
) -> CliResult<()> {
    let path = folder::dot_dir(root).join(folder::CONFIG_FILE);
    let bytes = std::fs::read(&path).code("config-corrupt", "missing key envelope")?;
    let head = ferry_crypto::config_head::parse_config_head(&bytes)
        .code("config-corrupt", "restore from backup")?;
    if head.entries.iter().any(|e| e.device_pub == *recipient) {
        return Ok(()); // already authorized
    }
    let mut entries: Vec<_> = head.entries.clone();
    entries.push(ferry_crypto::config_head::WrappedKeyEntry::new(
        *recipient, *wrapped,
    ));
    let updated = ferry_crypto::config_head::write_config_head(&folder_id, &entries);
    // Temp + rename so a crash cannot truncate the trust record.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &updated).code("io", format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).code("io", format!("cannot finalize {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// accept (device B)
// ---------------------------------------------------------------------------

pub fn accept(
    identity: &DeviceIdentity,
    offer_file: &Path,
    dir: Option<&Path>,
    timeout_secs: u64,
) -> CliResult<Output> {
    let offer_bytes = std::fs::read(offer_file).code(
        "not-found",
        "check the path to the offer file printed by `ferry pair`",
    )?;
    let offer = PairingOffer::parse(&offer_bytes).map_err(|e| {
        CliError::new(
            "bad-offer",
            e.to_string(),
            "get a fresh offer file from the sharing device",
        )
    })?;

    let target = dir.unwrap_or(Path::new("."));
    if folder::dot_dir(target).is_dir() {
        return Err(CliError::new(
            "already-initialized",
            format!("{} already contains a .ferry store", target.display()),
            "cd into the empty directory you want synced, or remove the old store deliberately",
        ));
    }

    // Show the human what to compare (informational over file transport).
    let expected_code = offer.short_code(TransportHints(0));

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
    eprintln!("Response written: {}", response_path.display());
    eprintln!("Expected short code: {expected_code} (compare against the other screen)");

    // Wait for the sealed grant.
    let grant_path = offer_file.with_file_name(GRANT_SUFFIX);
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let grant_bytes = loop {
        match std::fs::read(&grant_path) {
            Ok(bytes) => break bytes,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => {
                return Err(CliError::new(
                    "pair-timeout",
                    format!("no grant appeared within {timeout_secs}s at {}", grant_path.display()),
                    "make sure the other device completed `ferry pair` and copied pair-grant.ferry-grant beside the offer file",
                ))
            }
        }
    };

    let (folder_id, poly, wrapped_for_peer) = open_grant(&offer_bytes, &grant_bytes)?;
    let fmk = *unwrap_folder_key(&wrapped_for_peer, &folder_id, identity).map_err(|e| {
        CliError::new(
            "key-unwrap",
            e.to_string(),
            "the grant did not address this device; re-run the pairing",
        )
    })?;

    // Build the local store around the adopted key material.
    let store = folder::adopt_folder(target, identity, folder_id, &fmk, poly)?;
    store
        .flush()
        .map_err(|e| CliError::new("store", e.to_string(), "retry"))?;
    store
        .write_index_snapshot()
        .map_err(|e| CliError::new("store", e.to_string(), "retry"))?;
    let settings = Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: ferry_store::format::hex(&folder_id),
        honor_gitignore: false,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    save_settings(target, &settings)?;

    let json_doc = json!({
        "command": "pair",
        "role": "accept",
        "status": "completed",
        "folder": target.display().to_string(),
        "folder_id": ferry_store::format::hex(&folder_id),
        "device_id": ferry_store::format::hex(identity.public()),
        "expected_short_code": expected_code,
    });
    let human = format!(
        "Paired. Folder ready at {}.\nIt is currently empty — content arrives from the other device.\nNext:\n  ferry daemon --listen 127.0.0.1:<port>     (keep this side reachable), or\n  ferry sync --peer-url <other-device-address>",
        crate::commands::init::display_path(target)
    );
    Ok(Output::new(json_doc, human))
}

// ---------------------------------------------------------------------------
// grant sealing (A -> B handoff)
// ---------------------------------------------------------------------------

/// Key derivation and AEAD sealing live behind ferry-crypto (T-03): the CLI
/// only builds the JSON body and maps errors.
fn seal_grant(
    offer_bytes: &[u8],
    wrapped_for_peer: &[u8; WRAPPED_LEN],
    poly: u64,
) -> CliResult<Vec<u8>> {
    let body = json!({
        "wrapped_for_peer": hex_of(wrapped_for_peer),
        "poly": poly,
    })
    .to_string()
    .into_bytes();
    seal_pair_grant(offer_bytes, &body).map_err(grant_error)
}

fn open_grant(offer_bytes: &[u8], raw: &[u8]) -> CliResult<([u8; 16], u64, [u8; WRAPPED_LEN])> {
    let body = open_pair_grant(offer_bytes, raw).map_err(grant_error)?;
    let doc: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| CliError::new("bad-grant", "grant body unreadable", "redo the pairing"))?;
    let wrapped_hex = doc["wrapped_for_peer"]
        .as_str()
        .ok_or_else(|| CliError::new("bad-grant", "grant body incomplete", "redo the pairing"))?;
    let poly = doc["poly"]
        .as_u64()
        .ok_or_else(|| CliError::new("bad-grant", "grant body incomplete", "redo the pairing"))?;
    let wrapped = unhex_80(wrapped_hex)?;
    // folder_id rides in the offer itself (offsets pinned by ferry-crypto's
    // v1 offer layout); bounds-checked, no panic path.
    let folder_id: [u8; 16] = offer_bytes
        .get(5..21)
        .and_then(|s| <&[u8] as TryInto<[u8; 16]>>::try_into(s).ok())
        .ok_or_else(|| {
            CliError::new(
                "bad-grant",
                "the offer file is truncated",
                "get a fresh offer file from the sharing device",
            )
        })?;
    Ok((folder_id, poly, wrapped))
}

/// Map ferry-crypto's grant errors onto the CLI's targeted messages.
fn grant_error(e: GrantError) -> CliError {
    match e {
        GrantError::Malformed { .. } => CliError::new(
            "bad-grant",
            "the grant file is malformed",
            "have the other device re-run `ferry pair`",
        ),
        GrantError::Auth => CliError::new(
            "bad-grant",
            "the grant file failed authentication",
            "it must travel together with THIS exact offer file; redo the pairing",
        ),
        GrantError::OfferTruncated { .. } => CliError::new(
            "bad-grant",
            "the offer file is truncated",
            "get a fresh offer file from the sharing device",
        ),
        GrantError::Internal => CliError::new("crypto", "grant seal failed", "retry"),
    }
}

fn unhex_80(s: &str) -> CliResult<[u8; WRAPPED_LEN]> {
    ferry_store::format::unhex::<WRAPPED_LEN>(s).ok_or_else(|| {
        CliError::new(
            "bad-grant",
            "grant key envelope is not 160 hex chars",
            "redo the pairing",
        )
    })
}

fn hex_of(b: &[u8]) -> String {
    ferry_store::format::hex(b)
}

// ---------------------------------------------------------------------------
// QR rendering
// ---------------------------------------------------------------------------

/// Unicode half-block ASCII-art QR (with a one-module quiet zone). Terminal
/// width is irrelevant: modules print at half height via ▀▄█.
pub fn render_ascii_qr(bytes: &[u8]) -> CliResult<String> {
    let qr = qrcode_like_matrix(bytes)?;
    Ok(qr)
}

/// Build the matrix through ferry-crypto's qrcode dependency indirectly?
/// ferry-cli renders directly from the same crate version workspace-wide.
fn qrcode_like_matrix(bytes: &[u8]) -> CliResult<String> {
    let code = qrcode::QrCode::new(bytes).map_err(|e| {
        CliError::new(
            "qr",
            e.to_string(),
            "payload too unusual for a QR; use the offer file",
        )
    })?;
    let w = code.width();
    let colors = code.to_colors();
    debug_assert_eq!(colors.len(), w * w);

    let dark = |x: usize, y: usize| {
        colors
            .get(y * w + x)
            .is_some_and(|c| matches!(c, qrcode::Color::Dark))
    };
    let quiet = 1;
    let total = w + quiet * 2;

    let mut out = String::new();
    out.push('\u{2b}'); // '+' frame corner
    for _ in 0..total {
        out.push('\u{2500}'); // ─ top rule
    }
    out.push('\u{2b}');
    out.push('\n');
    let mut y = 0usize; // y indexes the QUIET-padded grid
    while y < total {
        let mut line = String::from('\u{2502}');
        let mut x = 0usize;
        while x < total {
            let mx = |yy: usize| x >= quiet && yy >= quiet && x < quiet + w && yy < quiet + w;
            let top = mx(y) && dark(x - quiet, y - quiet);
            let bottom = if y + 1 < total {
                mx(y + 1) && dark(x - quiet, y + 1 - quiet)
            } else {
                false
            };
            line.push(match (top, bottom) {
                (true, true) => '\u{2588}',  // █
                (true, false) => '\u{2580}', // ▀
                (false, true) => '\u{2584}', // ▄
                (false, false) => ' ',
            });
            x += 1;
        }
        line.push('\u{2502}');
        out.push_str(&line);
        out.push('\n');
        y += 2;
    }
    out.push('\u{2b}');
    for _ in 0..total {
        out.push('\u{2500}');
    }
    out.push('\u{2b}');
    Ok(out)
}
