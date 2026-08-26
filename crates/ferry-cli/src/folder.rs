//! Thin adapter over [`ferry-folder`], the one owner of folder bootstrap
//! (ticket 08). Names and signatures match what this crate always exposed;
//! the only additions are identity resolution (`FERRY_HOME`) and the
//! lossless conversion of coded library errors into [`CliError`]s.

use std::path::Path;

use ferry_crypto::folder_key::Fmk;
use ferry_crypto::identity::DeviceIdentity;
use ferry_ignore::FerryIgnore;
use ferry_store::store::Store;

use crate::error::{CliError, CliResult};

pub use ferry_folder::folder::{
    dot_dir, short_device, state_dir, OpenFolder, Settings, CONFIG_FILE, DEFAULT_FERRY_IGNORE,
    DOT_DIR, SETTINGS_FILE, SETTINGS_FORMAT_VERSION,
};

impl From<ferry_folder::FolderError> for CliError {
    fn from(e: ferry_folder::FolderError) -> Self {
        CliError::new(e.code, e.message, e.hint)
    }
}

/// Compile the folder's effective ignore rules from its settings on disk.
pub fn load_rules(folder: &Path, settings: &Settings) -> CliResult<FerryIgnore> {
    ferry_folder::folder::load_rules(folder, settings).map_err(CliError::from)
}

/// Create a brand-new synced folder at `root`. Fails when `.ferry` already
/// exists — never silently re-initialize trust material.
pub fn create_folder(
    root: &Path,
    identity: &DeviceIdentity,
    folder_id: [u8; 16],
    poly: u64,
) -> CliResult<(Store, Fmk)> {
    ferry_folder::folder::create_folder(root, identity, folder_id, poly).map_err(CliError::from)
}

/// Adopt an EXISTING folder key material into a fresh local store (the
/// `pair --accept` path): like [`create_folder`] but with a caller-supplied
/// FMK and only our own wrap written to `CONFIG_HEAD`.
pub fn adopt_folder(
    root: &Path,
    identity: &DeviceIdentity,
    folder_id: [u8; 16],
    fmk: &Fmk,
    poly: u64,
) -> CliResult<Store> {
    ferry_folder::folder::adopt_folder(root, identity, folder_id, fmk, poly).map_err(CliError::from)
}

/// Open an initialized folder under THIS device's identity (resolved from
/// `FERRY_HOME`, creating it on first use).
pub fn open_folder(root: &Path) -> CliResult<OpenFolder> {
    let identity = crate::ensure_identity()?;
    ferry_folder::folder::open_folder(root, &identity).map_err(CliError::from)
}

/// Find the polynomial record through the index (bootstrap step 3).
pub fn find_polynomial(store: &Store) -> CliResult<u64> {
    ferry_folder::folder::find_polynomial(store).map_err(CliError::from)
}

/// Write the default rule file unless one already exists.
pub fn write_default_ignore_if_absent(root: &Path) -> CliResult<bool> {
    ferry_folder::folder::write_default_ignore_if_absent(root).map_err(CliError::from)
}

/// Persist settings atomically enough for v0 (temp + rename).
pub fn save_settings(root: &Path, settings: &Settings) -> CliResult<()> {
    ferry_folder::folder::save_settings(root, settings).map_err(CliError::from)
}
