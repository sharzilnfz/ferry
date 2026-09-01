use std::fmt::Write as _;
use std::path::Path;

use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder;
use crate::out::Output;

struct Finding {
    path: String,
    line: Option<usize>,
    class: &'static str,
    preview: String,
}

pub fn run(folder: &Path, i_know: bool, timeout_secs: u64) -> CliResult<Output> {
    if let Ok(home) = crate::home::ferry_home() {
        let _ = crate::bootstrap::ensure_daemon(&home);
    }

    let opened = folder::open_folder(folder)?;
    let rules = folder::load_rules(&opened.root, &opened.settings)?;
    let warnings = ferry_ignore::secrets::scan_for_secrets(&rules, &opened.root);

    let findings: Vec<Finding> = warnings
        .iter()
        .map(|w| Finding {
            path: w.path.join("/"),
            line: w.line,
            class: w.class.label(),
            preview: w.preview.clone(),
        })
        .collect();

    if !findings.is_empty() && !i_know {
        let mut msg = format!(
            "{} secret risk(s) would SYNC to other devices:\n",
            findings.len()
        );
        for f in findings.iter().take(20) {
            let loc = f.line.map(|n| format!(":{n}")).unwrap_or_default();
            let _ = writeln!(
                msg,
                "  SECRET RISK [{}] {}{} — {}",
                f.class, f.path, loc, f.preview
            );
        }
        if findings.len() > 20 {
            let _ = writeln!(msg, "  … and {} more", findings.len() - 20);
        }
        let mut err = CliError::new(
            "secrets-found",
            msg.trim_end().to_string(),
            "review each path: exclude it (`ferry ignore '<pattern>'`) or accept the risk with --i-know",
        );
        err.detail = Some(json!({
            "warnings": findings
                .iter()
                .map(|f| json!({
                    "path": f.path,
                    "line": f.line,
                    "class": f.class,
                    "preview": f.preview,
                }))
                .collect::<Vec<_>>()
        }));
        return Err(err);
    }

    if let Some(out) = try_pairing_code_share(&opened, &findings, timeout_secs) {
        return out;
    }

    let identity = {
        let home = crate::home::ferry_home()?;
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)).map_err(|e| {
            CliError::new(
                "identity-corrupt",
                e.to_string(),
                "restore or replace your device.key",
            )
        })?
    };
    let mut out = super::pairing::initiate(&opened, &identity, timeout_secs)?;

    if let Some(obj) = out.json.as_object_mut() {
        obj.insert("command".into(), json!("share"));
        obj.insert("warnings_reviewed".into(), json!(!findings.is_empty()));
        obj.insert(
            "warnings".into(),
            json!(findings
                .iter()
                .map(|f| json!({
                    "path": f.path, "line": f.line, "class": f.class, "preview": f.preview,
                }))
                .collect::<Vec<_>>()),
        );
    }

    if !findings.is_empty() {
        out.human = format!(
            "Proceeding WITH {} flagged secret risk(s) (--i-know given):\n{}\n---\n{}",
            findings.len(),
            findings
                .iter()
                .map(|f| format!("  [{}] {}", f.class, f.path))
                .collect::<Vec<_>>()
                .join("\n"),
            out.human
        );
    }
    Ok(out)
}

fn try_pairing_code_share(
    opened: &folder::OpenFolder,
    findings: &[Finding],
    timeout_secs: u64,
) -> Option<CliResult<Output>> {
    let folder_id_hex = ferry_store::format::hex(&opened.folder_id);
    let home = crate::home::ferry_home().ok()?;
    // Ensure daemon is running so CreatePairingSession can be hosted persistently.
    let _ = crate::bootstrap::ensure_daemon(&home);
    // Ensure folder is registered in the device inventory so the daemon's
    // CreatePairingSession lookup (via FolderInventory) can resolve the path
    // from folder_id. `init` creates the store but does not auto-register.
    // Use ensure_registered with the real folder_id (not random) so the daemon
    // can find it by id.
    let _ = ferry_folder::inventory::FolderInventory::new(&home)
        .ensure_registered(&folder_id_hex, &opened.root);

    // Persistent daemon pairing service delegation (P-B1 / ticket 03):
    // ferry share must delegate to the daemon via CreatePairingSession IPC so the
    // offer survives CLI termination. The daemon hosts the rendezvous for the full TTL.
    let req = ferry_ipc::pairing::CreatePairingRequest::new(folder_id_hex.clone());
    let daemon_resp = crate::ipc::send_command(
        &opened.root,
        ferry_ipc::protocol::ClientCommand::CreatePairingSession { req },
    );

    let (code, expires_at) = match daemon_resp {
        Some(ferry_ipc::protocol::DaemonMessage::PairingCreated { response }) => {
            (response.code, response.expires_at)
        }
        _ => {
            // Fallback for environments without a running daemon (e.g., unit tests
            // with isolated FERRY_HOME where the binary cannot spawn). Uses in-process
            // ritual but is not the primary path; logs fallback for visibility.
            eprintln!("[share] daemon unavailable, falling back to in-process rendezvous");
            let identity =
                ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)).ok()?;
            let ritual = ferry_folder::pairing::PairingRitual::with_shared(
                home,
                identity,
                ferry_folder::pairing::shared_rendezvous(),
            );
            let pending = ritual.create_offer(opened).ok()?;
            let code = pending.short_code.clone();
            let expires_at = ferry_platform::time::fmt_rfc3339(
                pending
                    .expires_at
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or_else(|_| ferry_platform::time::now_unix().0),
            );
            let formatted = format_code(&code);
            let qr = render_code_qr(&code).unwrap_or_default();
            if !qr.is_empty() {
                eprintln!("{qr}");
            }
            eprintln!("Share code: {} (expires {})", formatted, expires_at);
            eprintln!(
                "On the other device run:\n  ferry join {} [DEST]",
                formatted
            );
            let json_doc = json!({
                "command": "share",
                "code": code.clone(),
                "expires_at": expires_at.clone(),
                "folder_id": folder_id_hex.clone(),
                "folder": opened.root.display().to_string(),
                "warnings_reviewed": !findings.is_empty(),
                "warnings": findings.iter().map(|f| json!({"path": f.path, "line": f.line, "class": f.class, "preview": f.preview})).collect::<Vec<_>>(),
            });
            let mut human = format!(
                "Share code: {} (expires {})\nFolder: {}\n",
                formatted,
                expires_at,
                opened.root.display()
            );
            if !findings.is_empty() {
                human = format!(
                    "Proceeding WITH {} flagged secret risk(s) (--i-know given):\n{}\n---\n{}",
                    findings.len(),
                    findings
                        .iter()
                        .map(|f| format!("  [{}] {}", f.class, f.path))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    human
                );
            }
            if timeout_secs > 0 {
                use std::io::Write;
                println!("{}", serde_json::to_string(&json_doc).unwrap());
                let _ = std::io::stdout().flush();
                // For fallback, poll the in-process map (legacy) and config head.
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
                let key: String = code.chars().filter(|c| *c != '-' && *c != ' ').collect();
                while std::time::Instant::now() < deadline {
                    // Prefer config-head check (daemon-independent) but also check map.
                    if let Ok(bytes) = std::fs::read(opened.root.join(".ferry/config")) {
                        if let Ok(head) = ferry_crypto::config_head::parse_config_head(&bytes) {
                            if head.entries.len() > 1 {
                                break;
                            }
                        }
                    }
                    let done_via_map = ferry_folder::pairing::shared_rendezvous()
                        .lock()
                        .map(|g| !g.contains_key(&key))
                        .unwrap_or(false);
                    if done_via_map {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            return Some(Ok(Output::new(json_doc, human)));
        }
    };

    let formatted = format_code(&code);
    let qr = render_code_qr(&code).unwrap_or_default();
    if !qr.is_empty() {
        eprintln!("{qr}");
    }
    eprintln!("Share code: {} (expires {})", formatted, expires_at);
    eprintln!(
        "On the other device run:\n  ferry join {} [DEST]",
        formatted
    );

    let json_doc = json!({
        "command": "share",
        "code": code.clone(),
        "expires_at": expires_at.clone(),
        "folder_id": folder_id_hex,
        "folder": opened.root.display().to_string(),
        "warnings_reviewed": !findings.is_empty(),
        "warnings": findings.iter().map(|f| json!({"path": f.path, "line": f.line, "class": f.class, "preview": f.preview})).collect::<Vec<_>>(),
    });
    let mut human = format!(
        "Share code: {} (expires {})\nFolder: {}\n",
        formatted,
        expires_at,
        opened.root.display()
    );
    if !findings.is_empty() {
        human = format!(
            "Proceeding WITH {} flagged secret risk(s) (--i-know given):\n{}\n---\n{}",
            findings.len(),
            findings
                .iter()
                .map(|f| format!("  [{}] {}", f.class, f.path))
                .collect::<Vec<_>>()
                .join("\n"),
            human
        );
    }

    if timeout_secs > 0 {
        use std::io::Write;
        println!("{}", serde_json::to_string(&json_doc).unwrap());
        let _ = std::io::stdout().flush();

        // Interactive wait loop per ticket 03: poll daemon-hosted session via
        // filesystem (config head gains peer) until deadline or peer joins.
        // This survives CLI termination because the daemon hosts the offer.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        while std::time::Instant::now() < deadline {
            if let Ok(bytes) = std::fs::read(opened.root.join(".ferry/config")) {
                if let Ok(head) = ferry_crypto::config_head::parse_config_head(&bytes) {
                    if head.entries.len() > 1 {
                        eprintln!("Peer joined — pairing completed.");
                        break;
                    }
                }
            }
            // Also consider daemon IPC poll as secondary signal (best-effort).
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    Some(Ok(Output::new(json_doc, human)))
}

fn format_code(code: &str) -> String {
    let upper = code.to_ascii_uppercase();
    if upper.len() == 6 {
        format!("{}-{}", &upper[..4], &upper[4..])
    } else if upper.len() > 4 {
        format!("{}-{}", &upper[..4], &upper[4..])
    } else {
        upper
    }
}

fn render_code_qr(code: &str) -> Result<String, String> {
    use qrcode::QrCode;
    let qr = QrCode::new(code.as_bytes()).map_err(|e| e.to_string())?;
    let s = qr
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Dark)
        .light_color(qrcode::render::unicode::Dense1x2::Light)
        .build();
    Ok(s)
}
