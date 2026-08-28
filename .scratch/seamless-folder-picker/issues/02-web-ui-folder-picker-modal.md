# 02: Interactive Folder Picker & Autocomplete in Web Dashboard

**What to build:** An interactive folder selection modal in the Web Dashboard. Users click "+ Add Folder" to explore local directories, search with path autocomplete, select quick locations (Home, Projects, Desktop), and trigger secret scan verification before adding a folder to sync.

**Blocked by:** None (can start immediately).

**Status:** closed

- [x] `/api/fs/ls` REST endpoint in the Web Dashboard server securely exposes filesystem navigation with path traversal guards.
- [x] Obsidian Glass modal renders breadcrumb navigation, directory lists, and quick-access directory presets.
- [x] Live input autocomplete suggests matching filesystem paths as the user types.
- [x] Secret detection warnings display in the modal if potential secrets exist in the selected folder.
- [x] Integration tests verify secure directory listing and folder addition from the web UI.
