use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderRecord {
    pub folder_id: String,
    pub path: PathBuf,
    pub added_at: String,
}

impl FolderRecord {
    #[must_use]
    pub fn new(folder_id: String, path: PathBuf, added_at: String) -> Self {
        Self {
            folder_id,
            path,
            added_at,
        }
    }
}

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
