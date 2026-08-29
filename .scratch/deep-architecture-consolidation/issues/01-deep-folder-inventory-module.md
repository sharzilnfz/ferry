# 01: Deep Folder Inventory Module

**What to build:** Consolidate folder registration, persistence, atomic file locking, path traversal guards, git status detection, and directory inspection into a single deep `FolderInventory` module in `ferry-folder`. Delete the shallow `FolderRecord` and dummy `FolderRegistry` in `ferry-ipc::registry` and the redundant TOML loader in `ferry-ipc::fs`.

**Blocked by:** None (can start immediately).

**Status:** in-review

- [x] Consolidate device-level folder registry persistence at `$FERRY_HOME/folders.toml` into `ferry-folder` with atomic temp-file replacement and file locking.
- [x] Implement `FolderInventory` interface: `register(path)`, `unregister(folder_id)`, `list()`, and `inspect_dir(path)`.
- [x] Move path traversal guards (`validate_and_normalize`), NFC unicode normalization, and git repository detection inside the inventory module.
- [x] Delete duplicate `load_folder_registry` in `ferry-ipc::fs` and shallow duplicate structs in `ferry-ipc::registry`.
- [x] Unit and integration tests verify registration, overlap prevention, atomic persistence, and directory inspection through the `FolderInventory` interface.
