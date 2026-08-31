pub mod error;
pub mod folder;
pub mod inventory;
pub mod pairing;

pub use error::{CodeInto, FolderError, FolderResult};

#[cfg(any(test, feature = "test-util"))]
pub use folder::open_or_create_test_store;
pub use folder::{
    is_initialized, load_ignore_policy, load_rules, open_folder, OpenFolder, Settings,
};
pub use inventory::{
    default_listing_root, ferry_home, sort_entries, validate_and_normalize, validate_path,
    DirectoryEntry, FolderInventory, FolderRecord, ListDirectoryRequest, ListDirectoryResponse,
};
