# Complete Two-Device Live Testing and Production Guide for Ferry

This guide explains how Ferry operates and how to verify every feature live across two physical devices.
The reference setup uses Machine A (MacBook) and Machine B (Arch Linux) connected over Tailscale.
All commands in this guide assume the `ferry` binary is installed in your system PATH.

---

## 1. System Topology and Architecture

Ferry connects physical machines directly over encrypted Tailscale WireGuard tunnels.

```
┌───────────────────────────────────────┐   Tailscale WireGuard Mesh   ┌───────────────────────────────────────┐
│               MACHINE A               │ ◄══════════════════════════► │               MACHINE B               │
│          (e.g. MacBook Air)           │                              │        (e.g. Arch Linux Laptop)       │
│                                       │                              │                                       │
│ • Tailscale IP: 100.91.38.24          │                              │ • Tailscale IP: 100.122.159.26        │
│ • Binary: ferry (in PATH)             │                              │ • Binary: ferry (in PATH)             │
│ • Test Folder: /tmp/ferry-sync-demo   │                              │ • Test Folder: /tmp/ferry-sync-demo   │
└───────────────────────────────────────┘                              └───────────────────────────────────────┘
```

### Centralized Daemon and State Storage

Ferry runs a user-level background daemon.
State is stored in two distinct locations:

1. **User Home Directory (`$FERRY_HOME` or `~/.ferry/`)**:
   - `daemon.sock`. The Unix domain socket for local IPC communication.
   - `identity`. The local Ed25519 cryptographic keypair for device authentication.
   - `folders.toml`. The registry of all registered folders.
   - `web_session.json`. Active web dashboard tokens and port metadata.

2. **Folder State Directory (`<folder>/.ferry/`)**:
   - `.ferry/config`. Cryptographic header containing the encrypted Folder Master Key (FMK) and wrapped keys per authorized device.
   - `.ferry/settings.toml`. Folder-level synchronization settings.
   - `.ferry/store/`. Content-addressable chunk store.
   - `.ferry/pin-state.json`. Active session hold and pinning status.
   - `.ferry/conflicts.jsonl`. Append-only log of quarantined conflict events.
   - `ferry.ignore`. Gitignore-compatible exclusion rules, located in the folder root next to `.ferry/`.

---

## 2. The Three UI Interfaces

Ferry provides three frontend interfaces that communicate with the daemon over local IPC:

1. **Desktop GUI (`ferry ui --gui`)**. Native desktop application with Obsidian Dark styling, glowing status beacons, peer tables, session pinning controls, and native OS folder picker dialogs.
2. **Terminal TUI (`ferry tui`)**. Retro terminal dashboard with live throughput meters, activity logs, keyboard navigation, and an in-terminal folder browser.
3. **Web Dashboard (`ferry ui --web`)**. Browser interface delivering real-time updates via Server-Sent Events (SSE), directory exploration, and token-based authentication.

---

## 3. Step-by-Step Live Testing Procedure

Execute these steps in order.
Verify the expected outcome at each step before proceeding.

---

### Step 1: Workspace Preparation

Create a clean test folder on both machines.

#### On Mac (Machine A):
```bash
rm -rf /tmp/ferry-sync-demo
mkdir -p /tmp/ferry-sync-demo
```

#### On Arch Linux (Machine B):
```bash
rm -rf /tmp/ferry-sync-demo
mkdir -p /tmp/ferry-sync-demo
```

---

### Step 2: Folder Initialization

Initialize Ferry in the test directory on Machine A.

#### On Mac (Machine A):
```bash
ferry init /tmp/ferry-sync-demo
```

#### Verification:
Inspect folder status on Machine A:
```bash
ferry status /tmp/ferry-sync-demo
```
The output displays the folder path, folder ID, device ID, and zero scanned files.
The cryptographic header `.ferry/config` exists on disk.

---

### Step 3: Zero-Config Over-the-Air Pairing

Ferry uses over-the-air pairing with automated network discovery.
No manual file copying, `scp`, or shared disks are required.

#### Action 3.1: Generate the pairing code on Mac (Machine A)
```bash
ferry share /tmp/ferry-sync-demo
```
This command performs an automated secret scan and outputs a 6-character short code (e.g. `K9X2-M4` or `K9X2M4`) along with an ASCII QR code.
The background daemon hosts the pairing rendezvous session automatically.

#### Action 3.2: Join on Arch Linux (Machine B)
On Arch Linux, run join with the printed code:
```bash
ferry join <CODE> /tmp/ferry-sync-demo
```
Machine B automatically discovers Machine A across the network, exchanges cryptographic keys, receives the encrypted Folder Master Key grant, and initializes the local folder.

#### Verification:
Check status on both machines:
```bash
ferry status --json /tmp/ferry-sync-demo
```
Both devices report the exact same `folder_id`.
Both devices now hold authorized key-wrap entries for each other.

---

### Step 4: Start Background Sync Daemons

Launch the daemons across the Tailscale network.

#### On Mac (Machine A - Listener):
```bash
ferry daemon --listen 0.0.0.0:44001 /tmp/ferry-sync-demo
```

#### On Arch Linux (Machine B - Dialer):
```bash
ferry daemon --peer-url 100.91.38.24:44001 --interval-secs 1 /tmp/ferry-sync-demo
```

#### Verification:
In another terminal on Arch Linux, inspect the peer state:
```bash
ferry status --json /tmp/ferry-sync-demo
```
The `peers` array is non-empty.
Each peer entry reports a valid `last_agreed_manifest_id` and connectivity status `reachable`.
Do not create test files until the first manifest agreement settles.

---

### Step 5: Verify Live File Synchronization

Test bidirectional synchronization and file property preservation.

#### Test 5.1: Create a file on Mac and verify on Arch
```bash
# On Mac:
echo "Hello from MacBook Air!" > /tmp/ferry-sync-demo/hello.txt

# On Arch Linux:
cat /tmp/ferry-sync-demo/hello.txt
```
Verify byte-for-byte checksum parity across both machines:
```bash
cksum /tmp/ferry-sync-demo/hello.txt
```
Both outputs match exactly.

#### Test 5.2: Create nested directory structures on Arch and verify on Mac
```bash
# On Arch Linux:
mkdir -p /tmp/ferry-sync-demo/backend/src
echo 'pub fn add(a: i32, b: i32) -> i32 { a + b }' > /tmp/ferry-sync-demo/backend/src/lib.rs

# On Mac:
cat /tmp/ferry-sync-demo/backend/src/lib.rs
```

#### Test 5.3: Sync large binary assets and executable permissions
```bash
# On Mac:
dd if=/dev/urandom of=/tmp/ferry-sync-demo/binary_blob.bin bs=1M count=4
printf '#!/bin/sh\necho "ferry executable check"\n' > /tmp/ferry-sync-demo/tool.sh
chmod 755 /tmp/ferry-sync-demo/tool.sh

# On Arch Linux:
sha256sum /tmp/ferry-sync-demo/binary_blob.bin
test -x /tmp/ferry-sync-demo/tool.sh && echo "Permission bit 755 preserved."
```

#### Test 5.4: Mutate an existing file
```bash
# On Mac:
echo "Appended live mutation" >> /tmp/ferry-sync-demo/hello.txt

# On Arch Linux:
tail -n 1 /tmp/ferry-sync-demo/hello.txt
```

#### Test 5.5: Verify default exclusion of `.env` files
```bash
# On Mac:
echo "SECRET_KEY=supersecret123" > /tmp/ferry-sync-demo/.env
sleep 2

# On Arch Linux:
ls /tmp/ferry-sync-demo/.env
```
The file does not exist on Arch Linux.
Default ignore rules protect sensitive environment files from syncing.

---

### Step 6: Session Pinning and Held Writes

Pinning grants temporary exclusive writer locks to one device.
Remote writes to pinned paths are held in quarantine until released.

```bash
# On Mac: Start a pin lock on the backend path
ferry pin start --paths 'backend/**' --hours 8 /tmp/ferry-sync-demo
test -f /tmp/ferry-sync-demo/.ferry/pin-state.json && echo "Pin state active."

# On Arch Linux: Attempt to edit inside the pinned tree
echo "Remote competing write" > /tmp/ferry-sync-demo/backend/src/remote.txt

# On Mac: Check held write status
ferry pin status /tmp/ferry-sync-demo
ls /tmp/ferry-sync-demo/backend/src/remote.txt
# Notice remote.txt is NOT present in the working tree.

# On Mac: Release the pin and reconcile
ferry pin release /tmp/ferry-sync-demo
ls /tmp/ferry-sync-demo/backend/src/remote.txt
# remote.txt is now reconciled into the tree.

# On Mac: Clean up the pin session
ferry pin stop /tmp/ferry-sync-demo
```

---

### Step 7: Desktop GUI Verification

Launch the desktop interface on Mac:
```bash
ferry ui --gui /tmp/ferry-sync-demo
```

#### Items to Verify:
1. An Obsidian Dark themed desktop window opens.
2. The Status Beacon at the top right glows **GREEN (SYNCED)**.
3. The peer table lists the connected Arch Linux machine with its device ID.
4. Click **Select Folder** in the top navigation. A native OS directory picker opens.
5. Click **Pin Session**, input `backend/**`, and confirm. The beacon turns **PURPLE (HOLDING)**.
6. Click **Pair Device** to display the QR code and pairing code modal.

---

### Step 8: Terminal TUI Dashboard Verification

Launch the retro terminal dashboard on Arch Linux:
```bash
ferry tui /tmp/ferry-sync-demo
```

#### Keyboard Controls to Verify:
- Press **`O`** or **`A`** to open the In-Terminal Folder Browser. Use arrow keys to navigate and `Esc` to dismiss.
- Press **`P`** to toggle session pinning hold.
- Press **`R`** to trigger an immediate folder rescan.
- Press **`C`** to view the Quarantined Conflicts modal.
- Press **`Q`** to exit cleanly and restore terminal cursor state.

---

### Step 9: Web Dashboard and Token Authentication

Start the web dashboard on Mac:
```bash
ferry ui --web --port 8080 --no-open /tmp/ferry-sync-demo
```

#### Retrieve Access Token:
In another terminal pane, retrieve the active web token:
```bash
ferry ui token /tmp/ferry-sync-demo
```

#### Items to Verify:
1. Open the printed URL `http://127.0.0.1:8080/?token=<token>` in your browser.
2. Verify live metrics, folder info, and connected peer status.
3. In a terminal, append text to a file:
   ```bash
   echo "Dashboard update test" >> /tmp/ferry-sync-demo/hello.txt
   ```
   The dashboard UI updates immediately via Server-Sent Events without a page refresh.
4. Verify token protection on API endpoints:
   ```bash
   # Unauthenticated request returns 401 or 403:
   curl -s -o /dev/null -w "%{http_code}\n" "http://127.0.0.1:8080/api/status"

   # Invalid token returns 401 or 403:
   curl -s -o /dev/null -w "%{http_code}\n" "http://127.0.0.1:8080/api/status?token=invalid"

   # Valid token returns 200:
   TOKEN=$(ferry ui token --json /tmp/ferry-sync-demo | jq -r .token)
   curl -s -w "\nHTTP: %{http_code}\n" "http://127.0.0.1:8080/api/status?token=${TOKEN}"
   ```

---

### Step 10: Conflict Handling and Quarantine

Ferry uses deterministic timestamp ordering for concurrent edits and quarantines losing versions.

1. Stop both daemons with `Ctrl+C` in their active terminals.
2. Write differing content to the same path on Mac:
   ```bash
   echo "Mac version $(date)" > /tmp/ferry-sync-demo/concurrent.txt
   ```
3. Write differing content to the same path on Arch Linux:
   ```bash
   echo "Arch version $(date)" > /tmp/ferry-sync-demo/concurrent.txt
   ```
4. Restart both daemons (Step 4 commands).
5. Wait for the sync round to finish.
6. Inspect conflicts on both machines:
   ```bash
   ferry conflicts list /tmp/ferry-sync-demo
   ```
7. Check the quarantine files on disk:
   ```bash
   ls -la /tmp/ferry-sync-demo/*.ferry-conflict.*
   ```
   The losing payload is fully preserved in the conflict quarantine file.
   No data is overwritten or merged destructively.

---

### Step 11: Secret Risk Gating

Ferry blocks pairing and sharing operations when uncommitted credentials or private keys are exposed.

1. Un-ignore `.env` temporarily:
   ```bash
   ferry ignore '!.env' /tmp/ferry-sync-demo
   ```
2. Write a fake API key:
   ```bash
   echo "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" >> /tmp/ferry-sync-demo/.env
   ```
3. Attempt to run share:
   ```bash
   ferry share /tmp/ferry-sync-demo
   ```
4. Ferry aborts with exit code `secrets-found`.
   The terminal displays a redacted preview of the secret and suggests adding an ignore rule.
5. Restore default ignore rules:
   ```bash
   sed -i '' '/^!\.env$/d' /tmp/ferry-sync-demo/ferry.ignore 2>/dev/null || sed -i '/^!\.env$/d' /tmp/ferry-sync-demo/ferry.ignore
   ```

---

### Step 12: Storage Garbage Collection

Prune unreachable historical packfiles from the content store.

1. Run dry-run garbage collection:
   ```bash
   ferry store gc --dry-run /tmp/ferry-sync-demo
   ```
2. Run live garbage collection with a zero grace period for immediate reclaim:
   ```bash
   ferry store gc --grace-secs 0 /tmp/ferry-sync-demo
   ```

---

### Step 13: Clean Teardown

Terminate daemons and clean up test fixtures on both machines.

1. Stop background daemons:
   ```bash
   ferry daemon stop
   ```
2. If running interactive foreground daemons, send `Ctrl+C` in their respective windows.
3. Confirm no lingering listener remains:
   ```bash
   lsof -nP -iTCP -sTCP:LISTEN | grep -E '44001|8080'
   ```
4. Remove temporary test directories:
   ```bash
   rm -rf /tmp/ferry-sync-demo
   ```

---

## 4. Troubleshooting and Diagnostics

| Symptom | Probable Cause | Corrective Action |
| :--- | :--- | :--- |
| `status` returns `not-a-folder` | Directory has not been initialized | Run `ferry init <folder>` |
| Empty `peers: []` array after pairing | Firewall or daemon unreachable | Verify Tailscale connection and check port 44001 with `nc -zv <IP> 44001` |
| Sync does not transfer files | First manifest agreement in progress | Wait one interval period and check `last_agreed_manifest_id` in `ferry status` |
| Web dashboard returns 401 or 403 | Missing or expired auth token | Retrieve current token with `ferry ui token <folder>` |
| Port 44001 address already in use | Previous daemon process still active | Run `ferry daemon stop` or terminate PID using `lsof -nP -iTCP:44001` |
| Secret scan false positive on share | Legitimate credentials in workspace | Exclude file via `ferry ignore '<pattern>'` or override with `ferry share --i-know` |

Universal health check command:
```bash
ferry status --json /tmp/ferry-sync-demo
```
A healthy folder returns JSON with `"command": "status"` and no top-level `"code"` error field.

---

## 5. Production Command Quick Reference

| Command | Machine | Description |
| :--- | :--- | :--- |
| `ferry init [folder]` | Machine A | Initializes folder cryptographic store and `.ferry/` header |
| `ferry share [folder]` | Machine A | Secret-scans, creates rendezvous pairing session, and prints short code + QR |
| `ferry join <code> [dest]` | Machine B | Discovers peer over network, performs cryptographic handshake, and adopts folder |
| `ferry daemon --listen <addr> [folder]` | Machine A | Starts sync listener daemon on specified address |
| `ferry daemon --peer-url <addr> [folder]` | Machine B | Starts sync dialer daemon targeting peer address |
| `ferry daemon stop` | Both | Gracefully terminates active background daemon |
| `ferry sync --peer-url <addr> [folder]` | Either | Executes one-shot synchronization pass |
| `ferry ui --gui [folder]` | Machine A | Opens native egui desktop interface |
| `ferry tui [folder]` | Either | Opens ratatui terminal dashboard |
| `ferry ui --web [--port <p>] [folder]` | Either | Starts web dashboard with live SSE updates |
| `ferry ui token [folder]` | Either | Retrieves URL and auth token for active web session |
| `ferry pin start --paths '<glob>' [folder]`| Either | Sets active writer lock holding remote writes |
| `ferry pin release [folder]` | Either | Reconciles held writes into working tree |
| `ferry pin stop [folder]` | Either | Disables pinning lock without reconciliation |
| `ferry conflicts list [folder]` | Either | Lists all quarantined conflict records |
| `ferry ignore [--list] [pattern] [folder]`| Either | Manages selective sync ignore rules and presets |
| `ferry store gc [--dry-run] [folder]` | Either | Collects and removes unreferenced blob packs |
| `ferry status [--json] [folder]` | Either | Reports folder health, peer links, and manifest status |
