use std::path::{Path, PathBuf};

use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::out::Output;

pub fn run(code: &str, dest: Option<&Path>) -> CliResult<Output> {
    let normalized = code.trim().to_string();
    if normalized.is_empty() {
        return Err(CliError::new(
            "bad-code",
            "pairing code is empty",
            "pass the 6-char code printed by ferry share",
        ));
    }
    let target = match dest {
        Some(p) => {
            if p.as_os_str().is_empty() {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            } else if p.is_relative() {
                std::env::current_dir()
                    .map(|cwd| cwd.join(p))
                    .unwrap_or_else(|_| p.to_path_buf())
            } else {
                p.to_path_buf()
            }
        }
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };

    if !target.exists() {
        std::fs::create_dir_all(&target).map_err(|e| {
            CliError::new(
                "bad-path",
                format!("cannot create {}: {e}", target.display()),
                "check permissions",
            )
        })?;
    }
    let canonical_target = std::fs::canonicalize(&target).unwrap_or(target.clone());

    let home = crate::home::ferry_home()?;
    let identity = ferry_crypto::identity::load_or_create(&crate::home::identity_root(&home))
        .map_err(|e| {
            CliError::new(
                "identity-corrupt",
                e.to_string(),
                "check $FERRY_HOME permissions",
            )
        })?;

    // One ritual, transport chosen internally: a 6-char code dials the
    // rendezvous; a payload path/`FERRY1:` envelope rides the file exchange.
    let ritual = ferry_folder::pairing::PairingRitual::with_shared(
        home,
        identity.clone(),
        ferry_folder::pairing::shared_rendezvous(),
    );
    let pending = ritual
        .accept_offer(&normalized, Some(&canonical_target))
        .map_err(cli_err)?;
    let result = pending.complete(0).map_err(cli_err)?;

    let folder_id = ferry_store::format::hex(&result.folder_id);
    let human = format!("Joined {} at {}\n", folder_id, canonical_target.display());
    eprintln!("{}", human.trim());

    let json_doc = json!({
        "command": "join",
        "folder_id": folder_id,
        "status": "joined",
        "code": normalized,
        "path": canonical_target.display().to_string(),
    });

    Ok(Output::new(json_doc, human))
}

fn cli_err(e: ferry_folder::FolderError) -> CliError {
    CliError::new(e.code, e.message, e.hint)
}
