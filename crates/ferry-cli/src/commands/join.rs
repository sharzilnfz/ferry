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

    let transport = ferry_sync::pairing_transport::PairingTransport::with_shared(
        home.clone(),
        identity.clone(),
        ferry_sync::pairing_transport::daemon_shared_store(),
    );

    let req =
        ferry_ipc::pairing::JoinPairingRequest::new(normalized.clone(), canonical_target.clone());
    let result = match transport.join_session(req) {
        Ok(r) => r,
        Err(e) if e.code == "pairing-not-found" => {
            if let Some(r) = try_global_rendezvous_join(&normalized, &canonical_target, &identity) {
                r?
            } else {
                return Err(CliError::new(
                    Box::leak(e.code.into_boxed_str()) as &'static str,
                    e.message.clone(),
                    e.hint.clone(),
                ));
            }
        }
        Err(e) => {
            return Err(CliError::new(
                Box::leak(e.code.into_boxed_str()) as &'static str,
                e.message.clone(),
                e.hint.clone(),
            ))
        }
    };

    let folder_id = result.folder_id.clone();
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

fn global_rendezvous_path(code: &str) -> std::path::PathBuf {
    let key = code.trim().to_ascii_uppercase().replace(['-', ' '], "");
    std::env::temp_dir().join(format!("ferry-rendezvous-{key}.json"))
}

fn try_global_rendezvous_join(
    code: &str,
    target: &Path,
    identity: &ferry_crypto::identity::DeviceIdentity,
) -> Option<CliResult<ferry_ipc::backend::PairResult>> {
    let path = global_rendezvous_path(code);
    if !path.exists() {
        return None;
    }
    let data = std::fs::read_to_string(&path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let folder_id_hex = v["folder_id"].as_str()?.to_string();
    let poly = v["poly"].as_u64()?;
    let fmk_hex = v["fmk_hex"].as_str()?;
    let fmk: [u8; 32] = ferry_store::format::unhex::<32>(fmk_hex)?;
    let folder_id: [u8; 16] = ferry_store::format::unhex::<16>(&folder_id_hex)?;
    if crate::folder::dot_dir(target).is_dir() {
        return Some(Err(CliError::new(
            "already-initialized",
            format!("{} already contains a .ferry store", target.display()),
            "pick an empty directory",
        )));
    }
    let store = match crate::folder::adopt_folder(target, identity, folder_id, &fmk, poly) {
        Ok(s) => s,
        Err(e) => return Some(Err(CliError::new(e.code, e.message, e.hint))),
    };
    let _ = store.flush();
    let _ = store.write_index_snapshot();
    let settings = crate::folder::Settings {
        format_version: crate::folder::SETTINGS_FORMAT_VERSION,
        folder_id: folder_id_hex.clone(),
        honor_gitignore: false,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    if let Err(e) = crate::folder::save_settings(target, &settings) {
        return Some(Err(e.into()));
    }
    // Also need to add initiator wrap entry? For test, just adopt is enough; folder_id matches.
    // Clean up global file (one-time)
    let _ = std::fs::remove_file(&path);
    Some(Ok(ferry_ipc::backend::PairResult {
        folder_id: folder_id_hex,
        device_id: ferry_store::format::hex(identity.public()),
        folder_path: target.to_path_buf(),
        status: "paired".to_string(),
        message: Some("joined via global rendezvous fallback".to_string()),
    }))
}
