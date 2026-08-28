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

pub fn run(folder: &Path, i_know: bool, _timeout_secs: u64) -> CliResult<Output> {
    let opened = match folder::open_folder(folder) {
        Ok(o) => o,
        Err(e) if e.code == "not-a-folder" => {
            super::init::run(folder, "init")?;
            folder::open_folder(folder)?
        }
        Err(e) => return Err(e),
    };
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
        // Object, not bare array: main.rs merges object details into the
        // stderr error document as { "warnings": [...] } per docs/cli-json.md.
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

    // Gate passed (or nothing found): ensure daemon and register folder.
    let _ = crate::ipc::ensure_daemon_running();
    let _ = crate::ipc::send_command_to_daemon(ferry_ipc::protocol::ClientCommand::RegisterFolder {
        path: opened.root.display().to_string(),
    });

    let identity = crate::ensure_identity()?;
    let folder_id_hex = ferry_store::format::hex(&opened.folder_id);

    let mut listen_addr_str = "127.0.0.1:0".to_string();
    if let Ok(addr_s) = std::fs::read_to_string(opened.state_dir().join("listen.addr")) {
        listen_addr_str = addr_s.trim().to_string();
    }
    let mut fmk_hex = String::new();
    if let Ok(head_bytes) = std::fs::read(opened.state_dir().join("config")) {
        if let Ok(head) = ferry_crypto::config_head::parse_config_head(&head_bytes) {
            if let Some(entry) = head.entries.iter().find(|e| e.device_pub == *identity.public()) {
                if let Ok(fmk) = ferry_crypto::folder_key::unwrap_folder_key(&entry.wrapped, &head.folder_id, &identity) {
                    fmk_hex = ferry_store::format::hex(fmk.as_ref());
                }
            }
        }
    }

    let mut code = ferry_ipc::backend::generate_6word_code();
    let pairing_resp = crate::ipc::send_command_to_daemon(
        ferry_ipc::protocol::ClientCommand::CreatePairingSession {
            folder_id: Some(folder_id_hex.clone()),
        },
    );
    if let Some(ferry_ipc::protocol::DaemonMessage::Ack {
        message: Some(ref msg),
        ..
    }) = pairing_resp
    {
        if let Ok(sess) = serde_json::from_str::<ferry_ipc::backend::PairingSession>(msg) {
            code = sess.code;
        }
    }

    if pairing_resp.is_none() {
        let session_id = format!("sess-{}", &ferry_store::format::hex(&rand::random::<[u8; 8]>())[..8]);
        let record = ferry_ipc::backend::PairingSessionRecord {
            session_id,
            code: code.clone(),
            folder_id: folder_id_hex.clone(),
            device_id: ferry_store::format::hex(identity.public()),
            listen_addr: listen_addr_str.clone(),
            poly: opened.poly,
            fmk_hex,
            created_sec: ferry_platform::time::now_unix().0,
            sync_listen_addr: Some(listen_addr_str),
        };
        let _ = ferry_ipc::backend::save_pairing_record(&record);
    }

    // Also write legacy offer file for compatibility
    if let Ok(pending) = ferry_folder::pairing::initiate_begin(&opened, &identity) {
        let _ = std::fs::write(&pending.offer_path, &pending.offer_bytes);
    }

    let json_doc = json!({
        "command": "share",
        "status": "advertising",
        "folder": opened.root.display().to_string(),
        "folder_id": folder_id_hex,
        "device_id": ferry_store::format::hex(identity.public()),
        "code": code,
        "warnings_reviewed": !findings.is_empty(),
        "warnings": findings
            .iter()
            .map(|f| json!({
                "path": f.path, "line": f.line, "class": f.class, "preview": f.preview,
            }))
            .collect::<Vec<_>>(),
    });

    let mut human = format!(
        "Sharing folder: {}\nFolder ID: {}\nPairing code: {}\n\nOn the other machine, run:\n  ferry join {} [dest]\n",
        opened.root.display(),
        folder_id_hex,
        code,
        code,
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

    Ok(Output::new(json_doc, human))
}
