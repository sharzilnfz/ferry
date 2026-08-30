//! `ferry init`: create a synced folder under this device's identity.

use std::path::Path;

use rand::rngs::StdRng;
use rand::SeedableRng;
use serde_json::json;

use crate::error::{CliError, CliResult};
use crate::folder::{
    self, dot_dir, save_settings, write_default_ignore_if_absent, Settings, SETTINGS_FORMAT_VERSION,
};
use crate::home;
use crate::out::Output;

pub fn run(path: &Path) -> CliResult<Output> {
    if path.exists() && !path.is_dir() {
        return Err(CliError::new(
            "not-a-directory",
            format!("{} exists and is not a directory", path.display()),
            "pass the folder you want to sync",
        ));
    }
    if dot_dir(path).is_dir() {
        return Err(CliError::new(
            "already-initialized",
            format!("{} already contains a .ferry store", path.display()),
            "run `ferry status` to inspect it, or pick another directory",
        ));
    }

    let home = home::ferry_home()?;
    let identity = ferry_crypto::identity::load_or_create(&home::identity_root(&home)).map_err(
        |e| {
            CliError::new(
                "identity-corrupt",
                e.to_string(),
                "your device.key is damaged; restore it from backup or delete it deliberately (this forks trust)",
            )
        },
    )?;

    // Fresh randomness for the two per-folder secrets: folder id + chunker
    // polynomial (irreducible by construction; see ferry-store::chunker).
    let mut seed = [0u8; 32];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::from_seed(seed));

    let mut folder_id = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut folder_id);

    let (store, _fmk) = folder::create_folder(path, &identity, folder_id, poly)?;
    let settings = Settings {
        format_version: SETTINGS_FORMAT_VERSION,
        folder_id: ferry_store::format::hex(&folder_id),
        honor_gitignore: false,
        presets: Vec::new(),
        overrides: Vec::new(),
    };
    save_settings(path, &settings)?;
    let ignore_written = write_default_ignore_if_absent(path)?;

    // Seal nothing yet — an empty folder has no blobs; the polynomial record
    // from create_folder is still sitting in staging. Flush so the store is
    // complete on disk even if the process dies right now.
    store.flush().map_err(|e| {
        CliError::new(
            "store",
            e.to_string(),
            "retry; if it persists the disk may be full",
        )
    })?;
    store.write_index_snapshot().map_err(|e| {
        CliError::new(
            "store",
            e.to_string(),
            "retry; if it persists check disk space",
        )
    })?;

    let device_id = ferry_store::format::hex(identity.public());
    let json_doc = json!({
        "command": "init",
        "folder": path.display().to_string(),
        "folder_id": settings.folder_id,
        "device_id": device_id,
        "created": true,
        "ignore_file_created": ignore_written,
    });

    let hint = next_step();
    let human = format!(
        "Initialized Ferry folder {}\
         \n  folder id  {}\
         \n  device     {}\
         \n  rules      ferry.ignore (defaults active; `ferry ignore --list`)\
         \n\n{hint}",
        display_path(path),
        settings.folder_id,
        device_id,
    );

    Ok(Output::new(json_doc, human))
}

fn next_step() -> String {
    "Next: `ferry pair` to connect another device and start syncing.\n\
         Or run `ferry --help` for the full five-minute walkthrough."
        .to_string()
}

/// Show `.` for an empty/relative-current path so hints read naturally.
pub fn display_path(p: &Path) -> String {
    if p.as_os_str().is_empty() {
        ".".to_string()
    } else {
        p.display().to_string()
    }
}
