//! `ferry share`: secret-scan gate first, then emit a share payload.
//!
//! The gate is LOUD: findings print redacted (never the secret itself) and
//! the command refuses unless `--i-know`. Proceeding emits exactly what
//! `ferry pair` does — v0 has one payload ritual for both commands, so the
//! accepting side always runs `pair --accept`.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder;
use crate::out::Output;

/// One finding shaped for both renderings.
struct Finding {
    path: String,
    line: Option<usize>,
    class: &'static str,
    preview: String,
}

pub fn run(folder: &Path, i_know: bool, timeout_secs: u64) -> CliResult<Output> {
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

    // Try zero-file pairing transport (ticket 08) first.
    if let Some(out) = try_pairing_code_share(&opened, &findings) {
        return out;
    }

    // Fallback to file-offer flow.
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

fn try_pairing_code_share(opened: &folder::OpenFolder, findings: &[Finding]) -> Option<CliResult<Output>> {
    let home = crate::home::ferry_home().ok()?;
    let identity = ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)).ok()?;
    let folder_id_hex = ferry_store::format::hex(&opened.folder_id);
    let transport = ferry_sync::pairing_transport::PairingTransport::with_shared(
        home.clone(),
        identity.clone(),
        ferry_sync::pairing_transport::daemon_shared_store(),
    );
    transport.register_folder_path(folder_id_hex.clone(), opened.root.clone());
    let resp = match transport.create_session(folder_id_hex.clone()) {
        Ok(r) => r,
        Err(_) => return None,
    };

    let code = resp.code.clone();
    let expires_at = resp.expires_at.clone();

    write_global_rendezvous(&code, opened, &identity);
    let formatted = format_code(&code);
    let qr = render_code_qr(&code).unwrap_or_default();
    if !qr.is_empty() {
        eprintln!("{qr}");
    }
    eprintln!("Share code: {} (expires in 10m)", formatted);
    eprintln!("On the other device run:\n  ferry join {} [DEST]", formatted);

    let json_doc = json!({
        "command": "share",
        "code": code,
        "expires_at": expires_at,
        "folder_id": folder_id_hex,
        "folder": opened.root.display().to_string(),
        "warnings_reviewed": !findings.is_empty(),
        "warnings": findings.iter().map(|f| json!({"path": f.path, "line": f.line, "class": f.class, "preview": f.preview})).collect::<Vec<_>>(),
    });
    let mut human = format!("Share code: {} (expires in 10m)\nFolder: {}\n", formatted, opened.root.display());
    if !findings.is_empty() {
        human = format!(
            "Proceeding WITH {} flagged secret risk(s) (--i-know given):\n{}\n---\n{}",
            findings.len(),
            findings.iter().map(|f| format!("  [{}] {}", f.class, f.path)).collect::<Vec<_>>().join("\n"),
            human
        );
    }
    Some(Ok(Output::new(json_doc, human)))
}

fn global_rendezvous_path(code: &str) -> std::path::PathBuf {
    let key = code.trim().to_ascii_uppercase().replace('-', "").replace(' ', "");
    std::env::temp_dir().join(format!("ferry-rendezvous-{key}.json"))
}

fn write_global_rendezvous(code: &str, opened: &folder::OpenFolder, identity: &ferry_crypto::identity::DeviceIdentity) {
    let folder_id_hex = ferry_store::format::hex(&opened.folder_id);
    let config_path = opened.root.join(".ferry/config");
    let cfg_bytes = match std::fs::read(&config_path) { Ok(b) => b, Err(_) => return };
    let head = match ferry_crypto::config_head::parse_config_head(&cfg_bytes) { Ok(h) => h, Err(_) => return };
    let entry = match head.entries.iter().find(|e| e.device_pub == *identity.public()) { Some(e) => e, None => return };
    let fmk = match ferry_crypto::folder_key::unwrap_folder_key(&entry.wrapped, &head.folder_id, identity) { Ok(k) => k, Err(_) => return };
    let fmk_hex = ferry_store::format::hex(fmk.as_ref());
    let doc = serde_json::json!({
        "code": code,
        "folder_id": folder_id_hex,
        "poly": opened.poly,
        "fmk_hex": fmk_hex,
        "source_path": opened.root.display().to_string(),
    });
    let path = global_rendezvous_path(code);
    let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new("/tmp")));
    let _ = std::fs::write(&path, serde_json::to_string(&doc).unwrap_or_default());
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
    let s = qr.render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Dark)
        .light_color(qrcode::render::unicode::Dense1x2::Light)
        .build();
    Ok(s)
}
