# Feature Specification: Seamless Onboarding & In-UI Directory Picker

Status: ready-for-agent

## Problem Statement

Setting up and testing Ferry currently requires a deep understanding of distributed systems, cryptographic key exchanges, network socket configuration, and multi-terminal orchestration.

1. **Manual File-Based Pairing:** Establishing trust between two devices requires a multi-step out-of-band exchange involving three separate cryptographic files (`pair-offer`, `pair-response`, `pair-grant`) manually copied across machines using `scp` or external drives.
2. **Explicit Network Flags:** Users must manually discover network interfaces, determine private or Tailscale IP addresses, select unused ports, and configure `--listen` and `--peer-url` CLI arguments.
3. **Folder-Locked Daemon Lifecycle:** Each daemon instance is bound to a single folder on startup. Users must run separate daemon and UI processes in different terminal panes for each folder they wish to sync.
4. **Lack of Interactive Filesystem Navigation:** Neither the Terminal TUI nor the Web Dashboard provides an interactive directory explorer. Selecting a folder requires typing exact absolute or relative paths in the terminal before launching the interface.

As a result, first-time onboarding and manual testing take over ten minutes and require extensive technical guidance, preventing friction-free adoption.

## Solution

Redesign the onboarding and folder management experience to enable complete zero-to-sync setup in under two minutes through:

1. **In-UI Directory Selection:** Enable interactive filesystem browsing and folder selection directly within the Terminal TUI (interactive tree browser), Web Dashboard (server-side filesystem explorer and path autocomplete), and Desktop GUI (native OS folder chooser dialog).
2. **Centralized Device Daemon:** Transition from folder-scoped daemons to a unified device-level daemon managing multiple synced folders through a central IPC socket.
3. **Zero-File In-Band Pairing:** Replace manual file transport with short numeric or passphrase pairing codes negotiated automatically over local mDNS or encrypted Iroh QUIC connections.
4. **Self-Bootstrapping Frontends:** Frontends automatically detect and spawn the background device daemon if it is not already running, eliminating the need to manage background processes manually.

## User Stories

1. As a developer testing Ferry for the first time, I want to run a single command (`ferry`) to launch the interface, so that I do not need to configure background daemons or port flags beforehand.
2. As a TUI user, I want to press a key (such as `A` or `O`) to open an interactive filesystem browser, so that I can visually navigate my directories and select a project folder to sync without typing long paths.
3. As a TUI user, I want arrow-key navigation, breadcrumb navigation, and directory filtering in the folder selector, so that I can quickly drill down to nested repositories.
4. As a Web UI user, I want a "+ Add Folder" action that opens a clean modal with recent project suggestions and a directory explorer, so that I can configure synced folders from my browser.
5. As a Web UI user, I want real-time server path validation and tab completion in the folder input, so that I avoid typos when entering folder locations.
6. As a Desktop GUI user, I want clicking "Select Folder" to launch the native macOS Finder or Linux directory dialog, so that folder selection feels completely native to my operating system.
7. As a developer sharing a folder, I want Ferry to generate a short, human-readable 6-word or 6-character pairing code, so that I can pair devices without creating or transferring `.ferry-pair` files.
8. As a developer accepting a folder share, I want to enter the pairing code and select a local destination directory, so that the devices automatically establish an encrypted peer connection and begin syncing.
9. As a developer on the same local area network or Tailscale mesh, I want devices to discover each other automatically via mDNS or peer discovery, so that I do not need to look up IP addresses.
10. As a developer managing multiple codebases, I want to switch between different synced folders within the same UI dashboard, so that I do not need to restart Ferry for each project.
11. As a developer adding a new folder, I want Ferry to perform an automatic pre-share secret scan, so that any exposed `.env` files or API keys are surfaced before synchronizing to peers.
12. As an engineer auditing active transfers, I want to see live per-folder sync progress bars, peer connectivity states, and chunk counts directly inside the active UI view.
13. As a developer encountering a file conflict, I want the UI to highlight the quarantined conflict file with options to inspect differences and resolve the winner, so that no manual file renaming is required.
14. As a developer holding edits during an active coding session, I want to toggle session pinning per folder directly in the UI, so that incoming remote changes are temporarily held.
15. As a developer closing the UI, I want background folder synchronization to continue uninterrupted, so that file changes remain in sync while I work in my editor.

## Implementation Decisions

### 1. Seams and Core Contracts

The primary architectural seam for all frontends is the `UiBackend` trait. All folder additions, filesystem traversals, and pairing handshakes flow through this interface:

- **Filesystem Traversal Contract:** Extend `UiBackend` with a `list_directory(path: Option<PathBuf>)` method returning directory entries (name, path, is_dir, is_symlink, is_git_repo, is_already_synced).
- **Multi-Folder Management Contract:** Extend `UiBackend` with methods to list active folders, register a new folder root, remove a folder from sync, and switch the active folder context.
- **In-Band Pairing Contract:** Extend `UiBackend` with `create_pairing_session(folder_id)` and `join_pairing_session(code, target_dir)` methods that handle the cryptographic handshake over the transport layer instead of on-disk payload files.

### 2. Device Daemon Architecture

- **Central IPC Socket:** The daemon binds to `$FERRY_HOME/daemon.sock` (or Windows named pipe) rather than `<folder>/.ferry/daemon.sock`.
- **Global Folder Registry:** The device daemon maintains a persistent folder registry at `$FERRY_HOME/folders.toml` mapping unique `FolderId`s to local filesystem paths and individual `SyncEngine` instances.
- **Independent Engine Execution:** Each registered folder runs its own isolated `SyncEngine` with its own content-addressed store, ignore rules, and session pin manager, while sharing the device's cryptographic identity and network transport.

### 3. Frontend Directory Selectors

- **Terminal TUI (`ferry-tui`):** Implement a modal filesystem explorer widget built with Ratatui. The widget renders directory hierarchies with folder icons, parent navigation (`..`), path search filter, and `[Space]` selection.
- **Web Dashboard (`ferry-daemon/ui`):** Add an `/api/fs/ls` REST endpoint querying the backend filesystem. The web frontend renders a responsive modal featuring quick-select presets (Home, Projects, Desktop), breadcrumb navigation, and inline subfolder expansion.
- **Desktop GUI (`ferry-gui`):** Integrate asynchronous native file dialogs via the `rfd` crate, allowing non-blocking folder selection on macOS, Linux, and Windows.

### 4. Zero-File Network Pairing Ritual

- **Ephemeral Rendezvous Codes:** When a share is initiated, the device generates a short, cryptographically secure rendezvous code derived from an ephemeral key exchange.
- **Direct & Relayed Discovery:** The connector dials the initiator using local mDNS advertisement on LAN or through the configured Iroh discovery relay using the pairing code as the rendezvous topic.
- **Automatic Key Material Envelope Exchange:** The three-way handshake (Offer -> Response -> Grant) executes entirely across the established QUIC stream, persisting the wrapped Folder Master Key (FMK) directly into the folder's `CONFIG_HEAD`.

## Testing Decisions

### 1. High-Level Seam Testing

- Test primarily through the `UiBackend` contract and IPC message protocol. Tests interact through `InProcessAdapter`, `DaemonIpcAdapter`, and `FakeBackend` to verify full feature behavior without flaky UI rendering checks.
- Do not assert on exact TUI terminal cell colors or Web DOM CSS classes; test state transitions, event emissions, and folder registration outcomes.

### 2. Module Verification Scope

- **`ferry-ipc`:** Unit and serialization tests for new directory browsing commands, multi-folder status snapshots, and pairing session RPCs.
- **`ferry-tui`:** Component tests simulating keyboard events (Arrow keys, Enter, Space, Filter typing) against `FakeBackend` to verify directory tree navigation and folder selection.
- **`ferry-daemon`:** Integration tests for `/api/fs/ls` API security (preventing path traversal exploits outside allowed roots), multi-folder engine lifecycle, and IPC dispatch.
- **`ferry-sync` & `ferry-crypto`:** End-to-end multi-device pairing test verifying that two in-memory or loopback devices successfully negotiate an encrypted folder sync using only a 6-word code.

### 3. Prior Art & Existing Test Harnesses

- Build upon `crates/ferry-ipc/tests/` for typed command dispatch tests.
- Re-use `scripts/quickstart-e2e.sh` and `scripts/skeleton-e2e.sh` conventions to create a new `scripts/zero-config-e2e.sh` validating the two-command share/join workflow.

## Out of Scope

- Cloud account authentication or central user identity servers (Ferry remains strictly local-first and peer-to-peer).
- Mobile device clients (iOS/Android).
- Full remote file manager editing capabilities (Ferry synchronizes directories; it is not a remote text editor).
- Partial file checkout or virtual filesystem mounting (VFS/FUSE).

## Further Notes

- Existing single-folder CLI commands (`ferry init <path>`, `ferry status <path>`) must remain fully functional for headless automation, scripting, and CI environments.
- When running inside headless environments (such as CI or remote SSH sessions without a TTY), directory selection gracefully falls back to explicit CLI path arguments.
