use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::backend::OpError;
use crate::registry::FolderRegistry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_git_repo: bool,
    pub is_already_synced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ListDirectoryRequest {
    pub path: Option<PathBuf>,
}

impl ListDirectoryRequest {
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDirectoryResponse {
    pub entries: Vec<DirectoryEntry>,
    pub absolute_path: PathBuf,
}

impl ListDirectoryResponse {
    #[must_use]
    pub fn new(entries: Vec<DirectoryEntry>, absolute_path: PathBuf) -> Self {
        Self {
            entries,
            absolute_path,
        }
    }
}

/// Resolve the default listing root when `path` is `None`.
///
/// Precedence: `$FERRY_HOME` (if set and non-empty) → `current_dir()` → `$HOME`/`.ferry` → `/tmp`.
#[must_use]
pub fn default_listing_root() -> PathBuf {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd;
    }
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        if !home.as_os_str().is_empty() {
            return home.join(".ferry");
        }
    }
    PathBuf::from("/tmp")
}

fn ferry_home_for_registry() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return Some(p);
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        if !home.as_os_str().is_empty() {
            return Some(home.join(".ferry"));
        }
    }
    None
}

/// Load `FolderRegistry` from `$FERRY_HOME/folders.toml` if present, else empty.
/// This is best-effort and never errors; parse failures return empty.
#[must_use]
pub fn load_folder_registry() -> FolderRegistry {
    let Some(home) = ferry_home_for_registry() else {
        return FolderRegistry::empty();
    };
    let path = home.join("folders.toml");
    let Ok(bytes) = std::fs::read_to_string(&path) else {
        return FolderRegistry::empty();
    };
    toml::from_str::<FolderRegistry>(&bytes).unwrap_or_else(|_| FolderRegistry::empty())
}

/// Returns true if `candidate` is ancestor or descendant of any registered folder.
#[must_use]
pub fn is_already_synced(candidate: &Path, registry: &FolderRegistry) -> bool {
    for rec in &registry.folders {
        let reg = rec.path.as_path();
        if candidate == reg {
            return true;
        }
        if candidate.starts_with(reg) || reg.starts_with(candidate) {
            return true;
        }
    }
    false
}

/// NFC-normalize a path string.
fn nfc_normalize_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().to_string();
    let nfc: String = s.nfc().collect();
    PathBuf::from(nfc)
}

/// Shared path traversal guard reused by `FakeBackend`, `InProcessAdapter`,
/// and ticket 06's web handler.
///
/// Checks:
/// - `None` → `default_listing_root()`
/// - rejects `//` (empty segment)
/// - NFC normalization
/// - absolute-only (`bad-path`)
/// - any `..` component → `path-traversal` with hint `path escapes allowed root`
pub fn validate_path(input: Option<PathBuf>) -> Result<PathBuf, OpError> {
    let raw = input.unwrap_or_else(default_listing_root);
    validate_and_normalize(raw)
}

/// Validate and normalize a concrete `PathBuf`.
pub fn validate_and_normalize(raw: PathBuf) -> Result<PathBuf, OpError> {
    let nfc_path = nfc_normalize_path(&raw);
    let s = nfc_path.to_string_lossy().to_string();
    if s.contains("//") {
        return Err(OpError::new(
            "bad-path",
            format!("path {} contains //", nfc_path.display()),
            "use single slashes",
        ));
    }

    if !nfc_path.is_absolute() {
        if nfc_path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(OpError::new(
                "path-traversal",
                format!("path {} escapes allowed root", nfc_path.display()),
                "path escapes allowed root",
            ));
        }
        return Err(OpError::new(
            "bad-path",
            format!("path {} is not absolute", nfc_path.display()),
            "use absolute path",
        ));
    }

    if nfc_path
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(OpError::new(
            "path-traversal",
            format!("path {} escapes allowed root", nfc_path.display()),
            "path escapes allowed root",
        ));
    }

    // Strip CurDir (.) and rebuild to ensure canonical form without duplicate slashes
    let mut cleaned = PathBuf::new();
    for comp in nfc_path.components() {
        match comp {
            Component::Prefix(p) => cleaned.push(p.as_os_str()),
            Component::RootDir => cleaned.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(OpError::new(
                    "path-traversal",
                    "path escapes allowed root",
                    "path escapes allowed root",
                ));
            }
            Component::Normal(s) => cleaned.push(s),
        }
    }
    if cleaned.as_os_str().is_empty() {
        cleaned.push("/");
    }
    Ok(cleaned)
}

/// Sort entries stably: directories first, then name ascending.
pub fn sort_entries(entries: &mut [DirectoryEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
}

/// Shared directory listing helper used by `InProcessAdapter` and the daemon IPC
/// handler. This keeps the real `read_dir` logic in one place (`ferry-ipc`).
/// The daemon's `InProcessAdapter` delegates here so `ferry-daemon` has no
/// duplicate `read_dir` implementation.
pub fn list_directory_sync(validated: PathBuf) -> Result<ListDirectoryResponse, OpError> {
    let registry = load_folder_registry();
    let meta = std::fs::metadata(&validated).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => OpError::new(
            "not-found",
            format!("no such directory: {}", validated.display()),
            "check path",
        ),
        std::io::ErrorKind::PermissionDenied => {
            OpError::new("permission-denied", e.to_string(), "check folder permissions")
        }
        _ => OpError::new("io", e.to_string(), "check folder permissions"),
    })?;
    if !meta.is_dir() {
        return Err(OpError::new(
            "not-a-directory",
            format!("not a directory: {}", validated.display()),
            "use a directory path",
        ));
    }
    let read_dir = std::fs::read_dir(&validated).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => {
            OpError::new("permission-denied", e.to_string(), "check folder permissions")
        }
        std::io::ErrorKind::NotFound => OpError::new(
            "not-found",
            format!("no such directory: {}", validated.display()),
            "check path",
        ),
        _ => OpError::new("io", e.to_string(), "check folder permissions"),
    })?;
    let mut entries: Vec<DirectoryEntry> = Vec::new();
    for entry_res in read_dir {
        let entry = entry_res.map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                OpError::new("permission-denied", e.to_string(), "check folder permissions")
            }
            _ => OpError::new("io", e.to_string(), "check folder permissions"),
        })?;
        let entry_path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let symlink_meta = std::fs::symlink_metadata(&entry_path).ok();
        let is_symlink = symlink_meta
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink());
        let is_dir = entry_path.is_dir();
        let is_git_repo = if is_dir {
            entry_path.join(".git").exists()
        } else {
            false
        };
        let is_already_synced = is_already_synced(&entry_path, &registry);
        entries.push(DirectoryEntry {
            name,
            path: entry_path,
            is_dir,
            is_symlink,
            is_git_repo,
            is_already_synced,
        });
    }
    sort_entries(&mut entries);
    Ok(ListDirectoryResponse::new(entries, validated))
}
