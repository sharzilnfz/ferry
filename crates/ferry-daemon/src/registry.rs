//! Device-level folder registry persisted at `$FERRY_HOME/folders.toml`.
//!
//! **Ownership choice:** `crates/ferry-daemon/src/registry.rs` (this file),
//! not `crates/ferry-folder/src/registry.rs`. Rationale: the supervisor
//! lives in `ferry-daemon` and needs standalone `$FERRY_HOME` resolution
//! without depending on `ferry-cli`. `ferry-folder` owns per-folder
//! bootstrap (`.ferry/config`) but not device home. Placing the registry
//! in `daemon` avoids a circular dep and keeps pure storage next to its
//! future consumer (ticket 07). Caller resolves `$FERRY_HOME` via
//! `ferry_cli::home::ferry_home()` and passes `&Path` to `load`/`save`
//! for testability. See commit b29e892.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use ferry_ipc::registry::FolderRecord;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FolderRegistry {
    #[serde(default)]
    pub folders: Vec<FolderRecord>,
}

impl FolderRegistry {
    #[must_use]
    pub fn new(folders: Vec<FolderRecord>) -> Self {
        Self { folders }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            folders: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegistryError {
    pub code: String,
    pub message: String,
    pub hint: String,
}

impl RegistryError {
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: hint.into(),
        }
    }

    fn corrupt(message: impl Into<String>, home: &Path) -> Self {
        Self::new(
            "corrupt-registry",
            message,
            format!("fix or delete {}/folders.toml", home.display()),
        )
    }

    fn bad_path(message: impl Into<String>) -> Self {
        Self::new(
            "bad-path",
            message,
            "provide an absolute path to an existing directory",
        )
    }

    fn already_synced(message: impl Into<String>) -> Self {
        Self::new(
            "already-synced",
            message,
            "folder already synced or contains a synced folder; pick a non-overlapping directory",
        )
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(
            "not-found",
            message,
            "check the folder_id and try again",
        )
    }

    fn io(err: std::io::Error) -> Self {
        Self::new("io", err.to_string(), "check permissions and disk space")
    }

    #[must_use]
    pub fn to_op_error(&self) -> ferry_ipc::backend::OpError {
        ferry_ipc::backend::OpError::new(
            self.code.clone(),
            self.message.clone(),
            self.hint.clone(),
        )
    }
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code={})\nhint: {}", self.message, self.code, self.hint)
    }
}

impl std::error::Error for RegistryError {}

fn is_hex(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_hexdigit())
}

fn validate_folder_id(id: &str) -> bool {
    (id.len() == 32 || id.len() == 64) && is_hex(id)
}

fn is_overlapping(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

fn sort_by_added_at(registry: &mut FolderRegistry) {
    registry.folders.sort_by(|a, b| {
        a.added_at
            .cmp(&b.added_at)
            .then_with(|| a.folder_id.cmp(&b.folder_id))
    });
}

fn validate_records(home: &Path, records: &[FolderRecord]) -> Result<(), RegistryError> {
    let mut seen_ids = HashSet::new();
    for r in records {
        if !validate_folder_id(&r.folder_id) {
            return Err(RegistryError::corrupt(
                format!("invalid folder_id {:?} (expected 32 or 64 hex chars)", r.folder_id),
                home,
            ));
        }
        if !seen_ids.insert(r.folder_id.clone()) {
            return Err(RegistryError::corrupt(
                format!("duplicate folder_id {}", r.folder_id),
                home,
            ));
        }
        if !r.path.is_absolute() {
            return Err(RegistryError::corrupt(
                format!("path is not absolute: {}", r.path.display()),
                home,
            ));
        }
        if !r.path.exists() {
            return Err(RegistryError::corrupt(
                format!("path does not exist: {}", r.path.display()),
                home,
            ));
        }
        if !r.path.is_dir() {
            return Err(RegistryError::corrupt(
                format!("path is not a directory: {}", r.path.display()),
                home,
            ));
        }
        if ferry_platform::time::parse_rfc3339_to_unix(&r.added_at).is_none() {
            return Err(RegistryError::corrupt(
                format!("invalid added_at {:?} for folder {}", r.added_at, r.folder_id),
                home,
            ));
        }
    }
    for i in 0..records.len() {
        for j in (i + 1)..records.len() {
            if is_overlapping(&records[i].path, &records[j].path) {
                return Err(RegistryError::corrupt(
                    format!(
                        "overlapping paths: {} and {}",
                        records[i].path.display(),
                        records[j].path.display()
                    ),
                    home,
                ));
            }
        }
    }
    Ok(())
}

impl FolderRegistry {
    #[must_use]
    pub fn sorted_list(&self) -> &[FolderRecord] {
        &self.folders
    }

    pub fn load(home: &Path) -> Result<Self, RegistryError> {
        let path = home.join("folders.toml");
        if !path.exists() {
            return Ok(Self::empty());
        }
        let content = std::fs::read_to_string(&path).map_err(RegistryError::io)?;
        let mut registry: Self = toml::from_str(&content).map_err(|e| {
            RegistryError::corrupt(format!("failed to parse {}: {e}", path.display()), home)
        })?;
        validate_records(home, &registry.folders)?;
        sort_by_added_at(&mut registry);
        Ok(registry)
    }

    pub fn save(&self, home: &Path) -> Result<(), RegistryError> {
        std::fs::create_dir_all(home).map_err(RegistryError::io)?;
        let mut sorted = self.clone();
        sort_by_added_at(&mut sorted);
        let content = toml::to_string(&sorted).map_err(|e| {
            RegistryError::corrupt(format!("failed to serialize registry: {e}"), home)
        })?;
        let mut tmp = tempfile::Builder::new()
            .prefix("folders")
            .suffix(".toml")
            .tempfile_in(home)
            .map_err(RegistryError::io)?;
        tmp.write_all(content.as_bytes()).map_err(RegistryError::io)?;
        tmp.flush().map_err(RegistryError::io)?;
        tmp.as_file().sync_all().map_err(RegistryError::io)?;
        let tmp_path = tmp.into_temp_path();
        std::fs::rename(tmp_path, home.join("folders.toml")).map_err(RegistryError::io)?;
        if let Ok(dir) = std::fs::File::open(home) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    pub fn register(&mut self, path: PathBuf) -> Result<FolderRecord, RegistryError> {
        if !path.is_absolute() {
            return Err(RegistryError::bad_path(format!(
                "path is not absolute: {}",
                path.display()
            )));
        }
        if !path.exists() {
            return Err(RegistryError::bad_path(format!(
                "path does not exist: {}",
                path.display()
            )));
        }
        if !path.is_dir() {
            return Err(RegistryError::bad_path(format!(
                "path is not a directory: {}",
                path.display()
            )));
        }
        for existing in &self.folders {
            if is_overlapping(&path, &existing.path) {
                return Err(RegistryError::already_synced(format!(
                    "path {} overlaps with already-synced {}",
                    path.display(),
                    existing.path.display()
                )));
            }
        }
        let folder_id = loop {
            let bytes: [u8; 32] = rand::random();
            let mut hex = String::with_capacity(64);
            for b in bytes {
                use std::fmt::Write as _;
                let _ = write!(hex, "{b:02x}");
            }
            if !self.folders.iter().any(|r| r.folder_id == hex) {
                break hex;
            }
        };
        let (secs, _) = ferry_platform::time::now_unix();
        let added_at = ferry_platform::time::fmt_rfc3339(secs);
        let record = FolderRecord::new(folder_id, path, added_at);
        self.folders.push(record.clone());
        sort_by_added_at(self);
        Ok(record)
    }

    pub fn remove(&mut self, folder_id: &str) -> Result<(), RegistryError> {
        let idx = self
            .folders
            .iter()
            .position(|r| r.folder_id == folder_id)
            .ok_or_else(|| {
                RegistryError::not_found(format!("folder_id not found: {folder_id}"))
            })?;
        self.folders.remove(idx);
        Ok(())
    }

    pub fn list(&self) -> &[FolderRecord] {
        &self.folders
    }
}
