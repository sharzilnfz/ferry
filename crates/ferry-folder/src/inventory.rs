use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::error::{FolderError, FolderResult};

const REGISTRY_FILE: &str = "folders.toml";

const LOCK_FILE: &str = "folders.toml.lock";

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

const LOCK_STALE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRecord {
    pub folder_id: String,
    pub path: PathBuf,
    pub added_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_git_repo: bool,
    pub is_already_synced: bool,

    pub is_initialized: bool,
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

#[must_use]
pub fn ferry_home() -> PathBuf {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
    {
        if !home.as_os_str().is_empty() {
            return home.join(".ferry");
        }
    }
    PathBuf::from("/tmp/.ferry")
}

fn nfc_normalize_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy().to_string();
    let nfc: String = s.nfc().collect();
    PathBuf::from(nfc)
}

pub fn validate_and_normalize(raw: PathBuf) -> FolderResult<PathBuf> {
    let nfc_path = nfc_normalize_path(&raw);
    let s = nfc_path.to_string_lossy().to_string();
    if s.contains("//") {
        return Err(FolderError::new(
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
            return Err(FolderError::new(
                "path-traversal",
                format!("path {} escapes allowed root", nfc_path.display()),
                "path escapes allowed root",
            ));
        }
        return Err(FolderError::new(
            "bad-path",
            format!("path {} is not absolute", nfc_path.display()),
            "use absolute path",
        ));
    }

    if nfc_path
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(FolderError::new(
            "path-traversal",
            format!("path {} escapes allowed root", nfc_path.display()),
            "path escapes allowed root",
        ));
    }

    let mut cleaned = PathBuf::new();
    for comp in nfc_path.components() {
        match comp {
            Component::Prefix(p) => cleaned.push(p.as_os_str()),
            Component::RootDir => cleaned.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(FolderError::new(
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

pub fn validate_path(input: Option<PathBuf>) -> FolderResult<PathBuf> {
    let raw = input.unwrap_or_else(default_listing_root);
    validate_and_normalize(raw)
}

pub fn sort_entries(entries: &mut [DirectoryEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
}

fn is_hex(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_hexdigit())
}

fn validate_folder_id(id: &str) -> bool {
    (id.len() == 32 || id.len() == 64) && is_hex(id)
}

fn is_overlapping(a: &Path, b: &Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
struct RegistryFile {
    #[serde(default)]
    folders: Vec<FolderRecord>,
}

fn corrupt(home: &Path, message: impl Into<String>) -> FolderError {
    FolderError::new(
        "corrupt-registry",
        message,
        format!("fix or delete {}/folders.toml", home.display()),
    )
}

fn validate_records(home: &Path, records: &[FolderRecord]) -> FolderResult<()> {
    let mut seen_ids = HashSet::new();
    for r in records {
        if !validate_folder_id(&r.folder_id) {
            return Err(corrupt(
                home,
                format!(
                    "invalid folder_id {:?} (expected 32 or 64 hex chars)",
                    r.folder_id
                ),
            ));
        }
        if !seen_ids.insert(r.folder_id.clone()) {
            return Err(corrupt(
                home,
                format!("duplicate folder_id {}", r.folder_id),
            ));
        }
        if !r.path.is_absolute() {
            return Err(corrupt(
                home,
                format!("path is not absolute: {}", r.path.display()),
            ));
        }
        if !r.path.exists() {
            return Err(corrupt(
                home,
                format!("path does not exist: {}", r.path.display()),
            ));
        }
        if !r.path.is_dir() {
            return Err(corrupt(
                home,
                format!("path is not a directory: {}", r.path.display()),
            ));
        }
        if ferry_platform::time::parse_rfc3339_to_unix(&r.added_at).is_none() {
            return Err(corrupt(
                home,
                format!(
                    "invalid added_at {:?} for folder {}",
                    r.added_at, r.folder_id
                ),
            ));
        }
    }
    for i in 0..records.len() {
        for j in (i + 1)..records.len() {
            if is_overlapping(&records[i].path, &records[j].path) {
                return Err(corrupt(
                    home,
                    format!(
                        "overlapping paths: {} and {}",
                        records[i].path.display(),
                        records[j].path.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn sort_by_added_at(records: &mut [FolderRecord]) {
    records.sort_by(|a, b| {
        a.added_at
            .cmp(&b.added_at)
            .then_with(|| a.folder_id.cmp(&b.folder_id))
    });
}

fn io_error(err: std::io::Error) -> FolderError {
    FolderError::new("io", err.to_string(), "check permissions and disk space")
}

struct RegistryLock {
    path: PathBuf,
}

impl RegistryLock {
    fn acquire(home: &Path) -> FolderResult<Self> {
        let path = home.join(LOCK_FILE);
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(FolderError::new(
                            "io",
                            "could not acquire folders.toml lock",
                            "another Ferry process may be writing; retry",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => return Err(io_error(e)),
            }
        }
    }

    fn is_stale(path: &Path) -> bool {
        let Ok(meta) = std::fs::metadata(path) else {
            return true;
        };
        match meta.modified() {
            Ok(mtime) => mtime.elapsed().is_ok_and(|age| age > LOCK_STALE),
            Err(_) => false,
        }
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub struct FolderInventory {
    home: PathBuf,
}

impl FolderInventory {
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }

    #[must_use]
    pub fn open() -> Self {
        Self::new(&ferry_home())
    }

    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    fn registry_path(&self) -> PathBuf {
        self.home.join(REGISTRY_FILE)
    }

    fn load_strict(&self) -> FolderResult<Vec<FolderRecord>> {
        let path = self.registry_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = std::fs::read_to_string(&path).map_err(io_error)?;
        let mut file: RegistryFile = toml::from_str(&content).map_err(|e| {
            corrupt(
                &self.home,
                format!("failed to parse {}: {e}", path.display()),
            )
        })?;
        validate_records(&self.home, &file.folders)?;
        sort_by_added_at(&mut file.folders);
        Ok(file.folders)
    }

    fn load_best_effort(&self) -> Vec<FolderRecord> {
        self.load_strict().unwrap_or_default()
    }

    fn persist(&self, records: &[FolderRecord]) -> FolderResult<()> {
        let mut sorted = records.to_vec();
        sort_by_added_at(&mut sorted);
        let body = toml::to_string(&RegistryFile { folders: sorted })
            .map_err(|e| corrupt(&self.home, format!("failed to serialize registry: {e}")))?;
        let mut tmp = tempfile::Builder::new()
            .prefix("folders")
            .suffix(".toml")
            .tempfile_in(&self.home)
            .map_err(io_error)?;
        tmp.write_all(body.as_bytes()).map_err(io_error)?;
        tmp.flush().map_err(io_error)?;
        tmp.as_file().sync_all().map_err(io_error)?;
        let tmp_path = tmp.into_temp_path();
        std::fs::rename(tmp_path, self.registry_path()).map_err(io_error)?;
        if let Ok(dir) = std::fs::File::open(&self.home) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    pub fn register(&self, path: &Path) -> FolderResult<FolderRecord> {
        std::fs::create_dir_all(&self.home).map_err(io_error)?;
        let _lock = RegistryLock::acquire(&self.home)?;
        let mut records = self.load_strict()?;
        if !path.is_absolute() {
            return Err(FolderError::new(
                "bad-path",
                format!("path is not absolute: {}", path.display()),
                "provide an absolute path to an existing directory",
            ));
        }
        if !path.exists() {
            return Err(FolderError::new(
                "bad-path",
                format!("path does not exist: {}", path.display()),
                "provide an absolute path to an existing directory",
            ));
        }
        if !path.is_dir() {
            return Err(FolderError::new(
                "bad-path",
                format!("path is not a directory: {}", path.display()),
                "provide an absolute path to an existing directory",
            ));
        }
        for existing in &records {
            if is_overlapping(path, &existing.path) {
                return Err(FolderError::new(
                    "already-synced",
                    format!(
                        "path {} overlaps with already-synced {}",
                        path.display(),
                        existing.path.display()
                    ),
                    "folder already synced or contains a synced folder; pick a non-overlapping directory",
                ));
            }
        }
        let folder_id = loop {
            let bytes: [u8; 32] = rand::random();
            let mut hex = String::with_capacity(64);
            for b in bytes {
                use std::fmt::Write as _;
                let _ = write!(hex, "{b:02x}");
            }
            if !records.iter().any(|r| r.folder_id == hex) {
                break hex;
            }
        };
        let (secs, _) = ferry_platform::time::now_unix();
        let added_at = ferry_platform::time::fmt_rfc3339(secs);
        let record = FolderRecord {
            folder_id,
            path: path.to_path_buf(),
            added_at,
        };
        records.push(record.clone());
        self.persist(&records)?;
        Ok(record)
    }

    pub fn unregister(&self, folder_id: &str) -> FolderResult<()> {
        std::fs::create_dir_all(&self.home).map_err(io_error)?;
        let _lock = RegistryLock::acquire(&self.home)?;
        let mut records = self.load_strict()?;
        let idx = records
            .iter()
            .position(|r| r.folder_id == folder_id)
            .ok_or_else(|| {
                FolderError::new(
                    "not-found",
                    format!("folder_id not found: {folder_id}"),
                    "check the folder_id and try again",
                )
            })?;
        records.remove(idx);
        self.persist(&records)?;
        Ok(())
    }

    pub fn list(&self) -> FolderResult<Vec<FolderRecord>> {
        self.load_strict()
    }

    pub fn inspect_dir(&self, path: Option<PathBuf>) -> FolderResult<ListDirectoryResponse> {
        let validated = validate_path(path)?;
        let registry = self.load_best_effort();
        let meta = std::fs::metadata(&validated).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => FolderError::new(
                "not-found",
                format!("no such directory: {}", validated.display()),
                "check path",
            ),
            std::io::ErrorKind::PermissionDenied => FolderError::new(
                "permission-denied",
                e.to_string(),
                "check folder permissions",
            ),
            _ => FolderError::new("io", e.to_string(), "check folder permissions"),
        })?;
        if !meta.is_dir() {
            return Err(FolderError::new(
                "not-a-directory",
                format!("not a directory: {}", validated.display()),
                "use a directory path",
            ));
        }
        let read_dir = std::fs::read_dir(&validated).map_err(|e| match e.kind() {
            std::io::ErrorKind::PermissionDenied => FolderError::new(
                "permission-denied",
                e.to_string(),
                "check folder permissions",
            ),
            std::io::ErrorKind::NotFound => FolderError::new(
                "not-found",
                format!("no such directory: {}", validated.display()),
                "check path",
            ),
            _ => FolderError::new("io", e.to_string(), "check folder permissions"),
        })?;
        let mut entries: Vec<DirectoryEntry> = Vec::new();
        for entry_res in read_dir {
            let entry = entry_res.map_err(|e| match e.kind() {
                std::io::ErrorKind::PermissionDenied => FolderError::new(
                    "permission-denied",
                    e.to_string(),
                    "check folder permissions",
                ),
                _ => FolderError::new("io", e.to_string(), "check folder permissions"),
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
            let is_already_synced = registry
                .iter()
                .any(|rec| is_overlapping(&entry_path, &rec.path));
            entries.push(DirectoryEntry {
                name,
                path: entry_path.clone(),
                is_dir,
                is_symlink,
                is_git_repo,
                is_already_synced,
                is_initialized: crate::folder::is_initialized(&entry_path),
            });
        }
        sort_entries(&mut entries);
        Ok(ListDirectoryResponse::new(entries, validated))
    }
}
