//! The pairing ritual as a CLI flow. The ritual itself (codes, envelopes,
//! files, ordering, wrap entries, grant sealing, transport selection) lives
//! in `ferry-folder`'s unified `PairingRitual`; this module adds only what
//! the CLI owns: QR art, stderr instructions, and the `{command: "pair"}`
//! output documents.

use std::path::Path;

use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder::OpenFolder;
use crate::out::Output;
use ferry_crypto::identity::DeviceIdentity;

/// Run the initiator side inside an opened folder. Prints the 6-char code +
/// ASCII QR + instructions BEFORE the payload file exists (so nothing races
/// a reader watching for it), then polls for the responder and completes
/// the FMK wrap.
pub fn initiate(
    opened: &OpenFolder,
    identity: &DeviceIdentity,
    timeout_secs: u64,
) -> CliResult<Output> {
    let ritual = ritual_for(identity)?;
    let pending = ritual.create_offer(opened)?;
    let qr = render_ascii_qr(&pending.payload)?;
    eprintln!("{qr}");
    eprintln!(
        "Share code (compare on the other device): {}\nOffer file: {}",
        pending.short_code,
        offer_path_display(&pending.payload_path)
    );
    eprintln!(
        "On the other device run:\n  ferry pair --accept {}",
        offer_path_display(&pending.payload_path)
    );

    let done = pending.complete(opened, identity, timeout_secs)?;

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

/// Run the acceptor side against an offer payload. The ritual detects the
/// input form (code, `FERRY1:` envelope, or payload file path) internally;
/// for the file transport this prints the response path + the expected code
/// on stderr, then waits for the sealed grant and adopts the folder.
pub fn accept(
    identity: &DeviceIdentity,
    offer_file: &Path,
    dir: Option<&Path>,
    timeout_secs: u64,
) -> CliResult<Output> {
    let ritual = ritual_for(identity)?;
    let pending = ritual.accept_offer(&offer_file.display().to_string(), dir)?;
    if let Some(ref response_path) = pending.response_path {
        eprintln!("Response written: {}", response_path.display());
    }
    eprintln!(
        "Expected short code: {} (compare against the other screen)",
        pending.expected_short_code
    );
    let expected_short_code = pending.expected_short_code.clone();
    let accepted = pending.complete(timeout_secs)?;

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

/// The unified ritual on this device's home + the process-wide rendezvous.
fn ritual_for(identity: &DeviceIdentity) -> CliResult<ferry_folder::pairing::PairingRitual> {
    let home = crate::home::ferry_home()?;
    Ok(ferry_folder::pairing::PairingRitual::with_shared(
        home,
        identity.clone(),
        ferry_folder::pairing::shared_rendezvous(),
    ))
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

/// Canonical display for an artifact path (falls back to raw on error).
fn offer_path_display(p: &Path) -> std::path::Display<'_> {
    let _ = p.canonicalize();
    p.display()
}
