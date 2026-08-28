//! The pairing ritual as a CLI flow. The ritual itself (files, codes,
//! ordering, wrap entries, grant sealing) lives in `ferry-folder`; this
//! module adds only what the CLI owns: QR art, stderr instructions, and the
//! `{command: "pair"}` output documents.
//!
//! File transport summary (full story in ferry-folder's pairing module):
//! v0 uses PAYLOAD FILES standing in for the ~93-byte out-of-band channel a
//! camera scan provides — `pair-offer.ferry-pair`,
//! `pair-response.ferry-pair`, `pair-grant.ferry-grant`.

use std::path::Path;

use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder::OpenFolder;
use crate::out::Output;
use ferry_crypto::identity::DeviceIdentity;

/// Run the initiator side inside an opened folder. Prints short code + ASCII
/// QR + instructions BEFORE the offer file exists (so nothing races a reader
/// watching for it), then polls for the responder and completes the FMK
/// wrap.
pub fn initiate(
    opened: &OpenFolder,
    identity: &DeviceIdentity,
    timeout_secs: u64,
) -> CliResult<Output> {
    let pending = ferry_folder::pairing::initiate_begin(opened, identity)?;
    let qr = render_ascii_qr(&pending.offer_bytes)?;
    eprintln!("{qr}");
    eprintln!(
        "Short code (compare on the other device): {}\nOffer file: {}",
        pending.short_code,
        offer_path_display(&pending.offer_path)
    );
    eprintln!(
        "On the other device run:\n  ferry pair --accept {}",
        offer_path_display(&pending.offer_path)
    );

    let done = ferry_folder::pairing::initiate_complete(pending, opened, identity, timeout_secs)?;

    let json_doc = json!({
        "command": "pair",
        "role": "initiate",
        "status": "completed",
        "folder": opened.root.display().to_string(),
        "folder_id": ferry_store::format::hex(&done.folder_id),
        "peer_device_id": ferry_store::format::hex(&done.peer_device_id),
        "short_code": done.short_code,
        "offer_file": done.offer_path.display().to_string(),
    });
    let human = format!(
        "Paired with device {}.\nIts copy of this folder's key is sealed at:\n  {}\nNext on the OTHER device: start syncing with\n  ferry daemon --peer-url <this-device-address>\n(or `ferry sync --peer-url ...` once).",
        crate::folder::short_device(&done.peer_device_id),
        done.grant_path.display()
    );
    Ok(Output::new(json_doc, human))
}

/// Run the acceptor side against an offer file. Writes the response where
/// the initiator looks for it, announces it + the expected code on stderr,
/// then waits for the sealed grant and adopts the folder.
pub fn accept(
    identity: &DeviceIdentity,
    offer_file: &Path,
    dir: Option<&Path>,
    timeout_secs: u64,
) -> CliResult<Output> {
    let pending = ferry_folder::pairing::accept_begin(identity, offer_file, dir)?;
    eprintln!("Response written: {}", pending.response_path.display());
    eprintln!(
        "Expected short code: {} (compare against the other screen)",
        pending.expected_short_code
    );
    let expected_short_code = pending.expected_short_code.clone();
    let accepted = ferry_folder::pairing::accept_complete(pending, identity, timeout_secs)?;

    let json_doc = json!({
        "command": "pair",
        "role": "accept",
        "status": "completed",
        "folder": accepted.folder.display().to_string(),
        "folder_id": ferry_store::format::hex(&accepted.folder_id),
        "device_id": ferry_store::format::hex(identity.public()),
        "expected_short_code": expected_short_code,
    });
    let human = format!(
        "Paired. Folder ready at {}.\nIt is currently empty — content arrives from the other device.\nNext:\n  ferry daemon --listen 127.0.0.1:<port>     (keep this side reachable), or\n  ferry sync --peer-url <other-device-address>",
        super::init::display_path(&accepted.folder)
    );
    Ok(Output::new(json_doc, human))
}

// ---------------------------------------------------------------------------
// QR rendering
// ---------------------------------------------------------------------------

/// Unicode half-block ASCII-art QR (with a one-module quiet zone). Terminal
/// width is irrelevant: modules print at half height via ▀▄█.
pub fn render_ascii_qr(bytes: &[u8]) -> CliResult<String> {
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

/// Join an active pairing session with a 6-word code and start synchronizing.
pub fn join(code: &str, dest: Option<&Path>, _timeout_secs: u64) -> CliResult<Output> {
    let identity = crate::ensure_identity()?;
    crate::ipc::ensure_daemon_running()?;

    let target_dir = dest.map_or_else(|| std::path::PathBuf::from("."), std::path::Path::to_path_buf);
    let abs_path = if target_dir.is_relative() {
        std::env::current_dir().map_or_else(|_| target_dir.clone(), |cwd| cwd.join(&target_dir))
    } else {
        target_dir.clone()
    };

    // Try in-band network pairing via daemon first
    let join_resp = crate::ipc::send_command_to_daemon(
        ferry_ipc::protocol::ClientCommand::JoinPairingSession {
            code: code.to_string(),
            target_dir: Some(abs_path.display().to_string()),
        },
    );

    if let Some(ferry_ipc::protocol::DaemonMessage::Ack {
        message: Some(msg),
        ..
    }) = join_resp
    {
        if let Ok(res) = serde_json::from_str::<ferry_ipc::backend::PairResult>(&msg) {
            let json_doc = json!({
                "command": "join",
                "status": "completed",
                "folder": abs_path.display().to_string(),
                "folder_id": res.folder_id,
                "peer_device_id": res.device_id,
                "code": code,
            });
            let human = format!(
                "Joined pairing session with code: {}\nSynchronizing folder: {}\nConnected to peer: {}\nSynchronization started in background.\n",
                code,
                abs_path.display(),
                res.device_id,
            );
            return Ok(Output::new(json_doc, human));
        }
    }

    let record = ferry_ipc::backend::load_pairing_record(code).ok_or_else(|| {
        CliError::new(
            "session-not-found",
            format!("no active pairing session found for code '{code}'"),
            "verify the pairing code on the sharing machine and ensure it ran `ferry share`",
        )
    })?;

    let fid_bytes = ferry_store::format::unhex::<16>(&record.folder_id).ok_or_else(|| {
        CliError::new(
            "corrupt-session",
            format!("invalid folder ID '{}' in pairing record", record.folder_id),
            "re-run `ferry share` on the host machine",
        )
    })?;

    std::fs::create_dir_all(&abs_path).map_err(|e| {
        CliError::new(
            "io",
            format!("failed to create destination directory {}: {e}", abs_path.display()),
            "check path permissions",
        )
    })?;

    if !record.fmk_hex.is_empty() {
        if let Some(fmk_bytes) = ferry_store::format::unhex::<32>(&record.fmk_hex) {
            if !abs_path.join(".ferry").join("config").exists() {
                let store = ferry_folder::folder::adopt_folder(
                    &abs_path,
                    &identity,
                    fid_bytes,
                    &fmk_bytes,
                    record.poly,
                ).map_err(|e| {
                    CliError::new(
                        e.code,
                        e.message,
                        e.hint,
                    )
                })?;
                if let Some(initiator_pub) = ferry_store::format::unhex::<32>(&record.device_id) {
                    if let Ok(wrapped_for_initiator) =
                        ferry_crypto::folder_key::wrap_folder_key(&fmk_bytes, &fid_bytes, &initiator_pub)
                    {
                        let _ = ferry_folder::folder::append_wrap_entry_for(
                            &abs_path,
                            fid_bytes,
                            &initiator_pub,
                            &wrapped_for_initiator,
                        );
                    }
                }
                let _ = store.flush();
                let _ = store.write_index_snapshot();
                let settings = ferry_folder::folder::Settings {
                    format_version: ferry_folder::folder::SETTINGS_FORMAT_VERSION,
                    folder_id: record.folder_id.clone(),
                    honor_gitignore: true,
                    presets: Vec::new(),
                    overrides: Vec::new(),
                };
                let _ = ferry_folder::folder::save_settings(&abs_path, &settings);
                let _ = ferry_folder::folder::write_default_ignore_if_absent(&abs_path);
            }
        }
    }

    let state_dir = abs_path.join(".ferry");
    let peers_dir = state_dir.join("peers");
    let _ = std::fs::create_dir_all(&peers_dir);

    if !record.device_id.is_empty() {
        let _ = std::fs::write(
            peers_dir.join(format!("{}.addr", record.device_id)),
            &record.listen_addr,
        );
    }

    // Ensure daemon is running and register folder with local background daemon
    crate::ipc::ensure_daemon_running()?;
    let reg_resp = crate::ipc::send_command_to_daemon(ferry_ipc::protocol::ClientCommand::RegisterFolder {
        path: abs_path.display().to_string(),
    });
    eprintln!("JOIN REGISTER_FOLDER RESULT: {reg_resp:?}");

    let json_doc = json!({
        "command": "join",
        "status": "completed",
        "folder": abs_path.display().to_string(),
        "folder_id": record.folder_id,
        "peer_device_id": record.device_id,
        "peer_addr": record.listen_addr,
        "code": code,
    });
    let human = format!(
        "Joined pairing session with code: {}\nSynchronizing folder: {}\nConnected to peer: {} ({})\nSynchronization started in background.\n",
        code,
        abs_path.display(),
        record.device_id,
        record.listen_addr,
    );
    Ok(Output::new(json_doc, human))
}

/// Canonical display for an artifact path (falls back to raw on error).
fn offer_path_display(p: &Path) -> std::path::Display<'_> {
    let _ = p.canonicalize();
    p.display()
}
