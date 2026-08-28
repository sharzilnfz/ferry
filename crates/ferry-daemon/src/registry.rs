//! Global folder registry for the centralized Ferry device daemon.
//!
//! Persists registered sync roots, metadata, and active folder status
//! under `$FERRY_HOME/folders.toml`.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

/// Returns the device home directory: `$FERRY_HOME` when set, else `$HOME/.ferry`.
#[must_use]
pub fn ferry_home() -> PathBuf {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    home.join(".ferry")
}

/// Returns the default path to `folders.toml` under `$FERRY_HOME`.
#[must_use]
pub fn default_folders_toml_path() -> PathBuf {
    ferry_home().join("folders.toml")
}

/// An entry in the global folder registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderEntry {
    /// 32 hex character folder ID.
    pub id: String,
    /// Local filesystem path for the sync root.
    pub path: PathBuf,
    /// Whether this folder is the currently active/selected context.
    #[serde(default)]
    pub active: bool,
    /// Current engine status if known (e.g., "idle", "syncing").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// Persistent registry of managed sync folders on this device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRegistry {
    /// List of registered folders.
    #[serde(default)]
    pub folders: Vec<FolderEntry>,
    /// ID of the currently active folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_folder_id: Option<String>,
}

impl FolderRegistry {
    /// Create a new empty folder registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the folder registry from a TOML file. Returns empty registry if the file does not exist.
    pub fn load_from_file(path: &Path) -> Result<Self, std::io::Error> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        let mut reg: Self = toml::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Synchronize active flags with active_folder_id
        if let Some(ref active_id) = reg.active_folder_id {
            for f in &mut reg.folders {
                f.active = f.id == *active_id;
            }
        } else if let Some(first) = reg.folders.first_mut() {
            first.active = true;
            reg.active_folder_id = Some(first.id.clone());
        }

        Ok(reg)
    }

    /// Save the registry to a TOML file atomically.
    pub fn save_to_file(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Register a folder ID and path. If already registered, updates path.
    pub fn register(&mut self, id: String, path: PathBuf) -> FolderEntry {
        if let Some(existing) = self.folders.iter_mut().find(|f| f.id == id) {
            existing.path.clone_from(&path);
            return existing.clone();
        }

        let is_first = self.folders.is_empty();
        let entry = FolderEntry {
            id: id.clone(),
            path,
            active: is_first,
            state: Some("idle".to_string()),
        };
        if is_first {
            self.active_folder_id = Some(id);
        }
        self.folders.push(entry.clone());
        entry
    }

    /// Unregister a folder by ID.
    pub fn unregister(&mut self, id: &str) -> Option<FolderEntry> {
        let idx = self.folders.iter().position(|f| f.id == id)?;
        let removed = self.folders.remove(idx);
        if self.active_folder_id.as_deref() == Some(id) {
            if let Some(first) = self.folders.first_mut() {
                first.active = true;
                self.active_folder_id = Some(first.id.clone());
            } else {
                self.active_folder_id = None;
            }
        }
        Some(removed)
    }

    /// Switch active folder context to `id`. Returns reference to newly active entry if found.
    pub fn switch(&mut self, id: &str) -> Option<&FolderEntry> {
        let exists = self.folders.iter().any(|f| f.id == id);
        if !exists {
            return None;
        }
        for f in &mut self.folders {
            f.active = f.id == id;
        }
        self.active_folder_id = Some(id.to_string());
        self.folders.iter().find(|f| f.id == id)
    }

    /// Return reference to the currently active folder.
    #[must_use]
    pub fn active_folder(&self) -> Option<&FolderEntry> {
        self.folders.iter().find(|f| f.active).or_else(|| self.folders.first())
    }

    /// Return a slice of all registered folders.
    #[must_use]
    pub fn list(&self) -> &[FolderEntry] {
        &self.folders
    }

    /// Find a folder entry by ID.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&FolderEntry> {
        self.folders.iter().find(|f| f.id == id)
    }

    /// Find a folder entry by path.
    #[must_use]
    pub fn find_by_path(&self, path: &Path) -> Option<&FolderEntry> {
        self.folders.iter().find(|f| f.path == path)
    }
}
