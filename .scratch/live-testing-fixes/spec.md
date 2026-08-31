# Unified Zero-Friction Network Pairing, Discovery, and Live Testing Remediation

Status: ready-for-agent

## Problem Statement

Developers working across multiple physical devices (such as macOS laptops and Linux workstations) face high friction and silent failures when synchronizing project directories with Ferry.

1. **Short-Code Sharing Fails Across Processes and Machines**: Running `ferry share` creates an ephemeral in-process pairing offer and exits immediately. When a developer attempts to join via `ferry join <CODE>` in another terminal or machine, the lookup fails with `pairing-not-found`. The sharer never adds the joiner's device public key to its `CONFIG_HEAD` allow-list, causing subsequent daemon sync handshakes to be rejected with unauthorized errors.
2. **Manual Network Configuration Overhead**: Users must manually look up IP addresses and supply `--listen 0.0.0.0:44001` and `--peer-url <IP>:44001` to establish connections. Daemons do not automatically discover and connect to authorized peers on the local network or Tailscale mesh.
3. **Dedicated Terminal Tab Burden**: Syncing requires maintaining an active terminal tab running `ferry daemon`. Running commands like `ferry share`, `ferry join`, or `ferry ui` without an existing daemon either fails or operates in a degraded mode.
4. **Web UI and GUI Zero-Friction Gap**: The Web dashboard and Desktop GUI do not support 1-step short-code network pairing or display discovered network peers for one-click connection.
5. **Held Manifest Storage Failure**: When incoming changes are held by a session pin, the daemon neglects to persist incoming remote manifest bytes into the store. Running `ferry pin release` subsequently fails with `held-manifest-missing`.
6. **TUI Pin Toggle and Disconnect Spam**: In the terminal TUI, pressing `P` on an active idle pin fails with `pin-active` errors instead of releasing the pin. When the daemon is offline, rapid reconnect attempts flood the activity log.
7. **Web UI Token Inaccessibility**: The web authentication token is only printed once on startup, making it inaccessible if the initial terminal output is scrolled or backgrounded.

## Solution

Ferry provides a zero-friction, AirDrop-like 1-step experience (`ferry share` to `ferry join <CODE>`) backed by automatic peer discovery, daemon auto-spawning, and resilient conflict handling.

1. **Network Rendezvous Pairing**: `ferry share` publishes an encrypted offer envelope to an Iroh network rendezvous topic derived from the 6-character code. `ferry join <CODE>` connects over the network, completes mutual cryptographic key exchange, updates `CONFIG_HEAD` allow-lists on both devices, and initiates sync immediately.
2. **Automatic Peer Discovery**: Daemons automatically advertise and discover authorized peers via local network mDNS and Iroh relay topics, establishing bidirectional QUIC sync tunnels without manual IP arguments.
3. **Transparent Daemon Auto-Start**: Commands (`ferry share`, `ferry join`, `ferry ui`, `ferry tui`) check daemon socket liveness and automatically spawn a detached background daemon process if one is not running.
4. **One-Click Frontend Pairing**: The Web dashboard and Desktop GUI feature simple 6-character code and QR sharing, join inputs, and a list of discovered nearby network peers with 1-click pairing.
5. **Robust Held Manifest Reconciliation**: The sync exchange engine persists remote manifest bytes into the store during holds, enabling clean three-way reconciliation and conflict quarantine on `ferry pin release`.
6. **Polished TUI and Token Inspection**: The TUI cleanly toggles pin state and throttles reconnect attempts when offline. A new `ferry ui token` CLI command inspects active web session credentials.

## User Stories

1. As a developer on Machine A, I want to run `ferry share ~/my-project` and receive a 6-character pairing code and QR code, so that I can easily connect another device.
2. As a developer on Machine B, I want to run `ferry join 7K9-PX2 ~/my-project` and have it automatically connect over the network, so that I do not have to copy pairing files manually.
3. As a developer joining a folder, I want both devices to automatically update their `CONFIG_HEAD` allow-lists with each other's device public key, so that subsequent daemon sync sessions authorize without manual intervention.
4. As a developer pairing two devices, I want pairing codes to expire cleanly after their validity window and be invalidated once consumed, so that pairing remains secure.
5. As a developer on a local WiFi network, I want running Ferry on both machines to discover each other via mDNS, so that I never have to look up or type local IP addresses.
6. As a developer on a Tailscale network, I want Ferry daemons to discover and connect to authorized peers automatically, so that synchronization happens without manual `--peer-url` flags.
7. As a developer running `ferry share`, `ferry join`, `ferry ui`, or `ferry tui`, I want Ferry to automatically launch the background daemon if it is not already running, so that I do not need a dedicated terminal tab.
8. As a developer inspecting background services, I want `ferry daemon status` and `ferry daemon stop` to accurately report and terminate the auto-spawned daemon process.
9. As a Web UI user, I want a "Share Folder" button that displays a 6-character code and QR code, so that I can initiate pairing from my browser.
10. As a Web UI user, I want a "Join with Code" input that accepts a 6-character code and destination path, so that I can adopt folders directly from the dashboard.
11. As a Web UI user, I want a "Discovered Devices" list showing nearby Ferry instances with a 1-click "Pair" button, so that I can connect trusted machines with a single click.
12. As a Desktop GUI user, I want pairing dialogs that display 6-character short codes and QR codes, so that the GUI experience matches the CLI.
13. As a developer with an active session pin, I want incoming remote changes held by the daemon to have their manifest bytes stored in the chunk store, so that reconciliation data is never lost.
14. As a developer running `ferry pin release`, I want held modifications reconciled against the baseline and local manifest without `held-manifest-missing` errors.
15. As a developer releasing a pin with conflicting remote edits, I want non-conflicting changes applied and conflicting files quarantined as `<file>.ferry-conflict.<device>-<timestamp>`, logged in `.ferry/conflicts.jsonl`.
16. As an automated script checking pin state, I want `ferry pin status --json` to report `{"held_changes": 0, "holding": false}` after release completes.
17. As a terminal TUI user, I want pressing `P` when a pin is active (even if holding is false) to release or stop the pin, so that I do not receive duplicate `pin-active` errors.
18. As a terminal TUI user opening the TUI before starting a daemon, I want the header to show `DISCONNECTED` with exponential backoff on retries, so that the activity log is not flooded with error messages.
19. As a developer running a background Web dashboard, I want to run `ferry ui token` to retrieve the active URL and access token, so that I can authenticate without searching startup logs.
20. As a developer running `ferry ui token` when no web server is active, I want a clear structured error `code: "no-active-web-ui"`, so that my scripts can detect server availability.
21. As a maintainer building the repository across macOS and Linux, I want `cargo check --all-targets` and `cargo test --workspace` to execute with 0 compiler warnings, so that build hygiene is maintained.

## Implementation Decisions

1. **P2P Rendezvous Channel**: Pairing offers are published to an encrypted rendezvous topic over Iroh derived from the 6-character base32 code. The sharer process or its background supervisor listens on the topic until the joiner responds, exchanges wrapped keys, and confirms completion.
2. **Mutual Key Wrap in Config Head**: Upon successful short-code handshake, the sharer wraps the Folder Master Key (FMK) for the joiner's device public key and atomically commits the entry into `CONFIG_HEAD`. The joiner similarly records the sharer's key wrap.
3. **Supervisor Autonomous Dialing**: The daemon supervisor reads authorized peer device IDs from `CONFIG_HEAD` for every registered folder and periodically queries the routing table (populated by mDNS and Iroh topic discovery) to dial known peers automatically.
4. **Daemon Auto-Spawn Helper**: A platform-level process management helper checks the Unix domain socket and PID lock file. If absent or dead, it launches `ferry daemon` in a detached background process and waits for socket readiness.
5. **Web and GUI Pairing Surface**: The Web dashboard API exposes endpoints for network pairing session creation, short-code joining, and discovered device enumeration, backed by Server-Sent Events (SSE) for live pairing progress.
6. **Manifest Storage Invariant**: In the sync exchange protocol, whenever incoming changes are held by an active pin, the remote manifest payload is immediately written to the content-addressed blob store before returning the held outcome.
7. **Session Metadata Persistence**: The web server writes its port, host, authentication token, and process ID to `.ferry/web_session.json` on boot and removes the file on clean termination.

## Testing Decisions

1. **External Behavior Focus**: Tests verify observable external contracts: CLI exit codes and JSON outputs, filesystem state on disk, network socket connectivity, and HTTP endpoint responses, without asserting private struct internals.
2. **Multi-Process Pairing Integration**: An end-to-end test spawns two isolated Ferry processes (`FERRY_HOME` A and B), executes `ferry share` on A and `ferry join <CODE>` on B over the network, and verifies mutual `CONFIG_HEAD` updates and subsequent automatic sync convergence.
3. **Autonomous Convergence Test**: A multi-device test launches two daemons with pre-paired folders and verifies that file modifications on Device A propagate to Device B over mDNS/Iroh discovery with zero explicit `--peer-url` parameters.
4. **Pin Release Conflict Test**: A test starts a session pin on Device A, simulates a concurrent edit on Device B, verifies that the change is held with its manifest in the store, and runs `ferry pin release` to verify quarantine file generation and clean reconciliation.
5. **TUI Key and Backoff Unit Tests**: Key handling unit tests verify that `P` dispatches `ReleasePin` on active pins, and event loop tests verify throttled reconnection upon stream disconnect.
6. **CLI Web Token Tests**: Tests verify `ferry ui token` output formatting against running web servers and structured error reporting when offline.
7. **Prior Art**: Tests build upon existing integration test suites in `crates/ferry-cli/tests/` and `crates/ferry-sync/tests/`.

## Out of Scope

- Multi-user access control or third-party team permission hierarchies.
- Cloud-hosted mandatory relay infrastructure (relays remain optional and self-hostable).
- File version history beyond three-way reconciliation against the last agreed manifest.
- Mobile native clients (iOS/Android).

## Further Notes

- Pairing codes strictly follow ADR-0006: 6-character base32 with CRC32 checksum and 24-hour expiration.
- Conflict handling strictly adheres to ADR-0004: never merge contents, always quarantine losers with timestamped filenames and log to `conflicts.jsonl`.

