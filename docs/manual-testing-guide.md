# Dual-Device Manual Testing and Production Verification Guide

This guide describes how to verify every Ferry feature across two physical or virtual devices.
The guide uses Device A (Local / Listener) and Device B (Remote / Dialer).
All commands assume the `ferry` binary is installed in your system PATH.

---

## 1. System Architecture and Storage Layout

Ferry synchronizes developer directories peer-to-peer with end-to-end encryption.
State is partitioned into local user state and folder state.

```
┌───────────────────────────────────────┐         Direct P2P / Relay Mesh         ┌───────────────────────────────────────┐
│               DEVICE A                │ ◄═════════════════════════════════════► │               DEVICE B                │
│             (e.g. macOS)              │                                         │             (e.g. Linux)              │
│                                       │                                         │                                       │
│ • Identity: Ed25519 in ~/.ferry/      │                                         │ • Identity: Ed25519 in ~/.ferry/      │
│ • State Root: $FERRY_HOME or ~/.ferry │                                         │ • State Root: $FERRY_HOME or ~/.ferry │
│ • Test Folder: /tmp/ferry-sync-demo   │                                         │ • Test Folder: /tmp/ferry-sync-demo   │
└───────────────────────────────────────┘                                         └───────────────────────────────────────┘
```

### Global User Storage (`$FERRY_HOME` or `~/.ferry/`)

- `identity/device.key`. Ed25519 private key for node identity.
- `identity/device.pub`. Ed25519 public key.
- `daemon.sock`. Local Unix domain socket (or Windows named pipe) for IPC.
- `daemon.pid`. Process ID and start token for supervisor management.
- `folders.toml`. Authoritative local registry of managed folders.
- `web_session.json`. Web dashboard tokens and port registrations.

### Per-Folder State (`<folder>/.ferry/`)

- `config`. Cryptographic header containing the encrypted Folder Master Key (FMK) and wrapped keys for authorized peers.
- `settings.json`. Local folder configuration and applied presets.
- `store/`. Content-addressable chunk store holding ChaCha20-Poly1305 encrypted packfiles at rest.
- `pin-state.json`. Active writer lock and session pinning metadata.
- `held/<peer-id>.jsonl`. Append-only ledger of incoming writes held during active pins.
- `conflicts.jsonl`. Append-only audit log of quarantined conflict events.
- `ferry.ignore`. Gitignore-compatible pattern rules, located in the folder root next to `.ferry/`.

---

## 2. Frontend Interfaces

Ferry provides three frontend interfaces communicating with the daemon over local IPC:

1. **Desktop GUI (`ferry ui --gui`)**. Native desktop window with Obsidian Dark styling, glowing status beacons (GREEN synced, PURPLE holding, AMBER scanning, RED error), live peer table, session pinning controls, and native OS folder picker dialogs.
2. **Terminal TUI (`ferry tui`)**. Ratatui retro terminal dashboard with live transfer meters, activity logs, folder browser (`O` or `A`), immediate rescan (`R`), pinning toggle (`P`), conflicts viewer (`C`), and clean exit (`Q`).
3. **Web Dashboard (`ferry ui --web`)**. Browser interface providing real-time Server-Sent Events (`/api/events`), token-authenticated REST endpoints (`/api/status`, `/api/conflicts`, `/api/pin/*`, `/api/fs/ls`), and directory exploration.

---

## 3. Step-by-Step Two-Device Testing Procedure

Run each step in order.
Verify the expected outcome at every step before continuing.

---

### Step 1: Clean Workspace Preparation

Create clean test directories on both devices.

#### On Device A:
```bash
rm -rf /tmp/ferry-sync-demo
mkdir -p /tmp/ferry-sync-demo
```

#### On Device B:
```bash
rm -rf /tmp/ferry-sync-demo
mkdir -p /tmp/ferry-sync-demo
```

---

### Step 2: Single-Surface Folder Initialization

Initialize Ferry in the test directory on Device A.

#### On Device A:
```bash
ferry init /tmp/ferry-sync-demo
```

#### Verification:
Inspect the folder status on Device A:
```bash
ferry status /tmp/ferry-sync-demo
```

Expected result:
- The output displays the folder path, folder ID (32 hex characters), and device ID (64 hex characters).
- `.ferry/config` exists on disk containing the sealed Folder Master Key.
- `ferry.ignore` exists in the folder root with starter ignore rules.

---

### Step 3: Zero-Config Over-the-Air Pairing

Ferry uses cryptographic shortcode rendezvous pairing over the network.
No manual key transfer, shared disk, or scp is required.

#### Action 3.1: Generate the share code on Device A
```bash
ferry share /tmp/ferry-sync-demo
```

Device A executes a pre-share secret scan, starts a rendezvous session, and displays an ASCII QR code along with a 6-character short code (for example: `K9X2-M4`).

#### Action 3.2: Join on Device B
On Device B, execute join with the printed code:
```bash
ferry join <CODE> /tmp/ferry-sync-demo
```

Device B contacts Device A via the rendezvous service, performs the 3-way cryptographic handshake (offer, response, grant), receives the wrapped Folder Master Key, and initializes its local encrypted store.

#### Verification:
Check status on both devices:
```bash
ferry status --json /tmp/ferry-sync-demo
```

Expected result:
- Both devices report the exact same `folder_id`.
- Both devices now register each other as authorized peers in `.ferry/config`.

---

### Step 4: Daemon Synchronization and Connectivity

Start background daemons on both machines or let Ferry manage them automatically.

#### Option 4.1: Direct Network Addressing (LAN / Tailscale / VPN)

On Device A (Listener):
```bash
ferry daemon --listen 0.0.0.0:44001 /tmp/ferry-sync-demo
```

On Device B (Dialer):
```bash
ferry daemon --peer-url <DEVICE_A_IP>:44001 --interval-secs 1 /tmp/ferry-sync-demo
```

#### Option 4.2: Local Discovery (mDNS)

When devices are on the same local subnet, mDNS automatically discovers and dials peers without explicit IP addresses.
Launch the background daemon or run any CLI command.

#### Verification:
Query status on Device B:
```bash
ferry status --json /tmp/ferry-sync-demo
```

Expected result:
- The `peers` array contains Device A's device ID.
- `connectivity` reports `reachable`.
- `last_agreed_manifest_id` shows the agreed root manifest.

---

### Step 5: Bidirectional Live Sync and Permission Preservation

Verify real-time synchronization, directory nesting, large assets, and file modes.

#### Test 5.1: Create a file on Device A and read on Device B
```bash
# On Device A:
echo "Hello from Device A!" > /tmp/ferry-sync-demo/hello.txt

# On Device B:
cat /tmp/ferry-sync-demo/hello.txt
```

Verify checksum equality:
```bash
cksum /tmp/ferry-sync-demo/hello.txt
```
Both devices output identical checksums.

#### Test 5.2: Create nested directories on Device B and read on Device A
```bash
# On Device B:
mkdir -p /tmp/ferry-sync-demo/backend/src
echo 'pub fn compute() -> i32 { 42 }' > /tmp/ferry-sync-demo/backend/src/lib.rs

# On Device A:
cat /tmp/ferry-sync-demo/backend/src/lib.rs
```

#### Test 5.3: Sync large binary payload and executable permissions
```bash
# On Device A:
dd if=/dev/urandom of=/tmp/ferry-sync-demo/binary_asset.bin bs=1M count=4
printf '#!/bin/sh\necho "ferry executable verification"\n' > /tmp/ferry-sync-demo/run.sh
chmod 755 /tmp/ferry-sync-demo/run.sh

# On Device B:
sha256sum /tmp/ferry-sync-demo/binary_asset.bin
test -x /tmp/ferry-sync-demo/run.sh && echo "Executable bit 755 preserved."
```

#### Test 5.4: Live in-place file mutation
```bash
# On Device A:
echo "Appended mutation line" >> /tmp/ferry-sync-demo/hello.txt

# On Device B:
tail -n 1 /tmp/ferry-sync-demo/hello.txt
```

---

### Step 6: Ignore Rules and Default Secret Protection

Ferry protects sensitive files from accidental synchronization.

#### Test 6.1: Verify default `.env` exclusion
```bash
# On Device A:
echo "DATABASE_URL=postgres://localhost:5432/db" > /tmp/ferry-sync-demo/.env
sleep 2

# On Device B:
ls /tmp/ferry-sync-demo/.env
```
Expected result: `ls` reports file not found on Device B. Built-in defaults exclude `.env`.

#### Test 6.2: Add custom ignore pattern
```bash
# On Device A:
ferry ignore '*.log' /tmp/ferry-sync-demo
echo "debug log entry" > /tmp/ferry-sync-demo/test.log
sleep 2

# On Device B:
ls /tmp/ferry-sync-demo/test.log
```
Expected result: `test.log` is ignored and not synced.

#### Test 6.3: Apply ignore preset
```bash
# On Device A:
ferry ignore --preset claude /tmp/ferry-sync-demo
ferry ignore --list /tmp/ferry-sync-demo
```
Expected result: Preset rules are appended and displayed in layer order.

---

### Step 7: Secret Risk Gating

Ferry blocks share and pairing operations when uncommitted secrets exist in the folder.

1. Temporarily un-ignore `.env`:
   ```bash
   ferry ignore '!.env' /tmp/ferry-sync-demo
   ```

2. Add a credential pattern:
   ```bash
   echo "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" >> /tmp/ferry-sync-demo/.env
   ```

3. Attempt to share the folder:
   ```bash
   ferry share /tmp/ferry-sync-demo
   ```
   Expected result: Ferry aborts with exit code 3 (`secrets-found`), displays a redacted finding preview, and suggests adding an ignore rule.

4. Test override flag:
   ```bash
   ferry share --i-know /tmp/ferry-sync-demo
   ```
   Expected result: The command proceeds past the warning gate. Cancel with `Ctrl+C`.

5. Restore ignore rules:
   ```bash
   sed -i '' '/^!\.env$/d' /tmp/ferry-sync-demo/ferry.ignore 2>/dev/null || sed -i '/^!\.env$/d' /tmp/ferry-sync-demo/ferry.ignore
   ```

---

### Step 8: Session Pinning and Held Writes

Session pinning establishes an exclusive writer lock.
Remote edits to pinned paths are quarantined in `.ferry/held/<peer>.jsonl` until released.

#### Action 8.1: Start writer lock on Device A
```bash
# On Device A:
ferry pin start --paths 'backend/**' /tmp/ferry-sync-demo
ferry pin status /tmp/ferry-sync-demo
```
Expected result: `state` reports `active` and `paths` lists `backend/**`.

#### Action 8.2: Produce competing remote edit on Device B
```bash
# On Device B:
echo "Remote conflicting edit during pin" > /tmp/ferry-sync-demo/backend/src/lib.rs
```

#### Action 8.3: Verify write is held on Device A
```bash
# On Device A:
ferry pin status /tmp/ferry-sync-demo
cat /tmp/ferry-sync-demo/backend/src/lib.rs
```
Expected result:
- `ferry pin status` reports `holding: true` with `held_changes > 0`.
- The local file on Device A remains unchanged. The remote edit is held in quarantine.

#### Action 8.4: Release pin and reconcile
```bash
# On Device A:
ferry pin release /tmp/ferry-sync-demo
cat /tmp/ferry-sync-demo/backend/src/lib.rs
```
Expected result:
- The held change is replayed through the 3-way reconcile engine and applied cleanly.
- The working tree updates to include the remote edit.

---

### Step 9: Conflict Handling and Quarantine

Ferry resolves concurrent competing edits deterministically using timestamps (ADR-0004).
The losing edit is quarantined beside the file with no destructive overwrites.

1. Stop sync daemons on both devices (`ferry daemon stop`).

2. Edit the same file simultaneously on both devices:
   ```bash
   # On Device A:
   echo "Version written on Device A at $(date)" > /tmp/ferry-sync-demo/conflict_test.txt

   # On Device B:
   echo "Version written on Device B at $(date)" > /tmp/ferry-sync-demo/conflict_test.txt
   ```

3. Restart daemons on both devices.

4. Query conflict records:
   ```bash
   ferry conflicts list /tmp/ferry-sync-demo
   ```

5. Inspect the quarantined file on disk:
   ```bash
   ls -la /tmp/ferry-sync-demo/conflict_test.txt.ferry-conflict.*
   ```
   Expected result:
   - One version wins based on timestamp precedence.
   - The losing version is saved as `conflict_test.txt.ferry-conflict.<device_id>-<timestamp>`.
   - `.ferry/conflicts.jsonl` logs the collision event.

---

### Step 10: Native Desktop GUI Verification

Launch the graphical interface on Device A:
```bash
ferry ui --gui /tmp/ferry-sync-demo
```

#### Verify the following GUI elements:
1. Window opens with Obsidian Dark theme styling (#161618 canvas, #7c3aed accent).
2. Status Beacon at the top right glows **GREEN (SYNCED)**.
3. Connected peers table displays Device B's device ID and reachability status.
4. Click **Select Folder** in the top navigation bar. The native OS file picker opens.
5. Click **Pin Session**, input `backend/**`, and confirm. The beacon turns **PURPLE (HOLDING)**.
6. Click **Pair Device**. A modal displays the 6-character code and pairing QR code.

---

### Step 11: Ratatui Terminal TUI Dashboard Verification

Launch the terminal interface on Device B:
```bash
ferry tui /tmp/ferry-sync-demo
```

#### Verify the following interactive keybindings:
- **`O` or `A`**: Opens the In-Terminal Folder Browser modal. Use arrow keys to navigate directories and press `Esc` to dismiss.
- **`P`**: Toggles session pinning hold on and off.
- **`R`**: Triggers an immediate change rescan of the working tree.
- **`C`**: Opens the Quarantined Conflicts viewer modal. Press `Esc` to close.
- **`Q`**: Exits the TUI cleanly, restoring cursor visibility and terminal mode.

---

### Step 12: Web Dashboard and Token-Authenticated REST / SSE API

Launch the Web UI on Device A:
```bash
ferry ui --web --port 8080 --no-open /tmp/ferry-sync-demo
```

#### Retrieve auth token:
```bash
ferry ui token /tmp/ferry-sync-demo
```

#### Verify Web UI and API security:
1. Open the browser at `http://127.0.0.1:8080/?token=<TOKEN>`.
2. Check that folder statistics, peer table, and transfer activity appear.
3. Test Server-Sent Events (SSE): In another terminal, run `echo "sse test" >> /tmp/ferry-sync-demo/hello.txt`. The web dashboard updates live without page reloads.
4. Test endpoint security:
   ```bash
   # Unauthenticated request returns 401 or 403:
   curl -s -o /dev/null -w "%{http_code}\n" "http://127.0.0.1:8080/api/status"

   # Authenticated request returns 200 OK:
   TOKEN=$(ferry ui token --json /tmp/ferry-sync-demo | jq -r .token)
   curl -s -w "\nHTTP: %{http_code}\n" "http://127.0.0.1:8080/api/status?token=${TOKEN}"
   ```

---

### Step 13: Content Store Garbage Collection

Reclaim unreferenced packfiles from the content-addressable store.

1. Execute a dry-run GC analysis:
   ```bash
   ferry store gc --dry-run /tmp/ferry-sync-demo
   ```
   Expected result: JSON output displays `scanned_packs`, `live_packs`, and `reclaimable_bytes` without deleting data.

2. Run live GC with zero grace period:
   ```bash
   ferry store gc --grace-secs 0 /tmp/ferry-sync-demo
   ```
   Expected result: Unreferenced historical packs are purged while live manifests and held changes are preserved.

---

### Step 14: Daemon Lifecycle and Teardown

Test daemon control and clean up test fixtures.

1. Query daemon supervisor status:
   ```bash
   ferry daemon status
   ```

2. Gracefully stop all background daemons:
   ```bash
   ferry daemon stop
   ```
   Expected result: Daemon terminates within deadline, and `daemon.sock` is unlinked.

3. Verify no lingering sockets or processes:
   ```bash
   ferry daemon status
   ```
   Expected result: Output reports `{"status": "stopped"}`.

4. Remove temporary test directories on both devices:
   ```bash
   rm -rf /tmp/ferry-sync-demo
   ```

---

## 4. Troubleshooting and Diagnostic Reference

| Error Code or Symptom | Root Cause | Solution |
| :--- | :--- | :--- |
| `not-a-folder` | Missing `.ferry/config` header | Run `ferry init <folder>` first. |
| `secrets-found` | Sensitive tokens or keys detected during share | Add pattern to `ferry.ignore` or pass `ferry share --i-know`. |
| `daemon-not-running` | Background daemon stopped | Run `ferry daemon start` or execute any command to auto-spawn. |
| `peers: []` empty in status | Firewall blocking traffic or peer offline | Verify network route and confirm port 44001 reachability. |
| `401 Unauthorized` on Web UI | Missing or incorrect session token | Query token with `ferry ui token <folder>` and include `?token=<TOKEN>`. |
| `pin-active` | Attempting to start pin while another session holds | Run `ferry pin status` or `ferry pin stop` to reset session. |

Diagnostic health check command:
```bash
ferry status --json /tmp/ferry-sync-demo
```
A healthy synchronized folder returns JSON with `"command": "status"` and no error code.

---

## 5. Command Reference Table

| Command | Role | Description |
| :--- | :--- | :--- |
| `ferry init [path]` | Device A | Initializes encrypted folder store and `.ferry/` header |
| `ferry share [path]` | Device A | Performs secret scan, hosts pairing session, prints shortcode and QR |
| `ferry join <code> [dest]` | Device B | Handshakes with peer, receives Folder Master Key, adopts folder |
| `ferry daemon --listen <addr> [path]` | Device A | Runs background sync engine listening for incoming connections |
| `ferry daemon --peer-url <addr> [path]` | Device B | Runs background sync engine dialing peer address |
| `ferry daemon start` | Either | Starts central background device daemon |
| `ferry daemon stop` | Either | Gracefully terminates active background daemon |
| `ferry daemon status` | Either | Queries background daemon supervisor status and PID |
| `ferry sync --peer-url <addr> [path]` | Either | Executes one-shot convergence cycle |
| `ferry status [--json] [path]` | Either | Reports folder status, peer reachability, and manifest IDs |
| `ferry pin start --paths '<glob>' [path]`| Either | Locks paths and holds competing remote writes |
| `ferry pin release [path]` | Either | Reconciles and applies held remote writes |
| `ferry pin stop [path]` | Either | Ends pinning session without reconciling |
| `ferry pin status [path]` | Either | Inspects active writer lock and held changes |
| `ferry conflicts list [path]` | Either | Lists quarantined conflict records |
| `ferry ignore [pattern] [path]` | Either | Adds exclusion rule to `ferry.ignore` |
| `ferry ignore --preset <name> [path]` | Either | Applies preset rules (`claude`, `opencode`) |
| `ferry ignore --list [path]` | Either | Displays active ignore layers and precedence |
| `ferry store gc [--dry-run] [path]` | Either | Identifies and collects unreferenced store packs |
| `ferry ui --gui [path]` | Either | Opens native desktop application window |
| `ferry tui [path]` | Either | Opens Ratatui terminal dashboard |
| `ferry ui --web [--port <p>] [path]` | Either | Starts web dashboard with live SSE streaming |
| `ferry ui token [path]` | Either | Retrieves authentication token for active web session |
