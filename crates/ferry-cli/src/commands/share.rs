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

    // Gate passed (or nothing found): emit the payload via the pairing flow.
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

    // Re-shape the document as a share.
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

    // Prepend the warning recap to the human text even when proceeding.
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
