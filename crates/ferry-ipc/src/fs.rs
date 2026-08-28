use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
