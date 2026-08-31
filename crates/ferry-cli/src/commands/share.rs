






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
    let home = crate::home::ferry_home().ok()?;
    let identity =
        ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home)).ok()?;
    let folder_id_hex = ferry_store::format::hex(&opened.folder_id);
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
    eprintln!("Share code: {} (expires in 10m)", formatted);
    eprintln!(
        "On the other device run:\n  ferry join {} [DEST]",
        formatted
    );

    let json_doc = json!({
        "command": "share",
        "code": code,
        "expires_at": expires_at,
        "folder_id": folder_id_hex,
        "folder": opened.root.display().to_string(),
        "warnings_reviewed": !findings.is_empty(),
        "warnings": findings.iter().map(|f| json!({"path": f.path, "line": f.line, "class": f.class, "preview": f.preview})).collect::<Vec<_>>(),
    });
    let mut human = format!(
        "Share code: {} (expires in 10m)\nFolder: {}\n",
        formatted,
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

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        let key: String = code.chars().filter(|c| *c != '-' && *c != ' ').collect();
        while std::time::Instant::now() < deadline {
            let map = ferry_folder::pairing::shared_rendezvous();
            if let Ok(guard) = map.lock() {
                if !guard.contains_key(&key) {
                    break;
                }
            }
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
