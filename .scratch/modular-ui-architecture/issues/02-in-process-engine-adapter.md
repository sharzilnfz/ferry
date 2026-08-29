# 02: In-Process Engine Adapter (`InProcessAdapter`)

**What to build:** An in-process implementation of `UiBackend` that directly queries local folders using `ferry-folder`, `ferry-scan`, `ferry-pin`, and `ferry-sync-engine`. This allows standalone CLI commands and UI instances to operate without a running daemon process while completely eliminating the ~600 lines of duplicate disk-scanning fallback routines in `ferry-daemon/src/ui/backend.rs`.

**Blocked by:** None (can start immediately)

**Status:** closed

- [x] `InProcessAdapter` satisfies the `UiBackend` trait by directly invoking `ferry-folder::folder::open_folder`, `ferry-scan::ScanEngine`, and `AgreementLedger`.
- [x] The ~600 lines of duplicated disk fallback helpers (`read_status_from_disk`, `read_conflicts_from_disk`, `share_folder_disk`, `pair_accept_disk`, `pin_start_disk`, etc.) in `ferry-daemon/src/ui/backend.rs` are deleted and routed through `InProcessAdapter`.
- [x] Running a one-shot query against a local test fixture folder yields an accurate `EngineSnapshot` identical in structure to the daemon snapshot.
- [x] Secret scanning and pairing operations executed in-process enforce identical safety checks (`--i-know` gating) as the CLI commands.
