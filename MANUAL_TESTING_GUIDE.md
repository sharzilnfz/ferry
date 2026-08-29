# The Complete Guide to Ferry: Architecture, Concepts and Manual Testing

Welcome to Ferry. This guide explains how Ferry works, where each component lives, and how to verify every feature across macOS and Linux.

---

## 1. The Big Picture

Ferry is a fast, encrypted, peer-to-peer folder synchronization engine built for developers.

- **The Problem**. You write code on macOS, but compile and run tests on an Arch Linux laptop. Moving code with Git commits or third-party clouds adds latency and leaks secrets.
- **The Solution**. Ferry continuously watches a local workspace directory. When you edit a file, Ferry splits changes into content-addressed chunks, encrypts them with a shared folder key, and beams them over your private Tailscale network in milliseconds.

---

## 2. System Topology and Architecture

Ferry connects two physical or virtual machines directly over Tailscale WireGuard tunnels.

```
┌────────────────────────────────────────────────────────┐   Tailscale WireGuard Mesh   ┌────────────────────────────────────────────────────────┐
│                      MACBOOK AIR                       │ ◄══════════════════════════► │                   ARCH LINUX LAPTOP                    │
│                                                        │                              │                                                        │
│ • IP: 100.91.38.24                                     │                              │ • IP: 100.122.159.26                                   │
│ • Code Repo: /Users/sharzilnafis/Projects/dumps/idea2  │                              │ • Code Repo: /home/sharzil/Projects/dumps/ferry        │
│ • Binary: ./target/release/ferry                       │                              │ • Binary: ~/.cargo/bin/ferry                           │
│ • Herdr Pane: RIGHT PANE                               │                              │ • Herdr Pane: LEFT PANE (ssh sharzil@sharzilx)         │
│ • Test Folder: /tmp/ferry-sync-demo                    │                              │ • Test Folder: /tmp/ferry-sync-demo                    │
└────────────────────────────────────────────────────────┘                              └────────────────────────────────────────────────────────┘
```

### Herdr Terminal Setup
- **Left Pane**. Connected to your Arch Linux machine over SSH (`sharzil@sharzilx`).
- **Right Pane**. Connected to your local macOS shell.

### Centralized Device Daemon and Folder Structure
Ferry runs a centralized device daemon per user.
- `$FERRY_HOME/daemon.sock`. IPC Unix domain socket used by CLI and frontends to communicate with the supervisor.
- `$FERRY_HOME/folders.toml`. Registry recording all synced folders, folder IDs, and root paths.
- `$FERRY_HOME/identity`. The device Ed25519 cryptographic keypair.

Inside each synced folder (e.g. `/tmp/ferry-sync-demo`), Ferry maintains a hidden `.ferry/` state directory:
- `.ferry/config`. Cryptographic header containing the encrypted Folder Master Key (FMK).
- `.ferry/settings.toml`. Folder-specific synchronization settings and ignore rules.
- `.ferry/store/`. Content-addressable chunk store.
- `.ferry/pin-state.json`. Active session hold and pinning status.

---

## 3. The Three UI Faces

Ferry provides three frontend interfaces connected to the daemon over IPC:

```
                               ┌────────────────────────────────┐
                               │          Ferry Daemon          │
                               │  (Watches files, transfers)    │
                               └───────────────┬────────────────┘
                                               │
               ┌───────────────────────────────┼───────────────────────────────┐
               ▼                               ▼                               ▼
       ┌───────────────┐               ┌───────────────┐               ┌───────────────┐
       │  Desktop GUI  │               │ Terminal TUI  │               │ Web Dashboard │
       │ `ui --gui`    │               │    `tui`      │               │ `ui --web`    │
       └───────────────┘               └───────────────┘               └───────────────┘
```

1. **Desktop GUI (`ferry ui --gui`)**. Native macOS/Linux desktop application built with `egui`. Features Obsidian Dark styling, glowing status beacons, peer table, session pinning controls, and native OS folder selection dialogs via `rfd`.
2. **Terminal TUI (`ferry tui`)**. Retro terminal dashboard built with `ratatui`. Features live throughput meters, activity logs, keyboard shortcuts (`O`, `P`, `R`, `C`, `Q`), and an in-terminal filesystem browser.
3. **Web Dashboard (`ferry ui --web`)**. Browser interface built with `axum`. Delivers real-time updates via Server-Sent Events (SSE), directory exploration, and token-based URL authentication.

---

## 4. Step-by-Step Hands-On Tutorial

Follow these steps to test Ferry from scratch.

---

### Step 1: Prepare the Test Environment

Create a clean demo workspace directory on both computers.

#### Run on Mac (Right Pane):
```bash
mkdir -p /tmp/ferry-sync-demo
```

#### Run on Arch Linux (Left Pane):
```bash
mkdir -p /tmp/ferry-sync-demo
```

---

### Step 2: Initialize Ferry Workspace on Mac

Like `git init`, Ferry works either directly inside the current directory or by passing an explicit path.

#### Option A: In-directory execution (like `git init`):
```bash
cd /tmp/ferry-sync-demo
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry init
```

#### Option B: Explicit path execution:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry init /tmp/ferry-sync-demo
```

Both options generate the `.ferry/` repository directory, cryptographic identity, and chunk store.

---

### Step 3: 30-Second Seamless Onboarding (Zero-File Pairing)

Ferry provides a zero-file rendezvous workflow. You share a 6-character code instead of copying manual files.

```
       Mac (Sharer / Initiator)                           Arch Linux (Joiner / Acceptor)
   ┌──────────────────────────────┐                     ┌──────────────────────────────┐
   │ 1. `ferry share`             │                     │                              │
   │    • Scans for secrets       │                     │                              │
   │    • Emits short code & QR   │ ── Share code ────► │ 2. `ferry join <code>`       │
   │      (e.g., 7KQ4-2M)         │    (voice / chat)   │    • Auto-provisions store   │
   │                              │                     │    • Adopts folder ID & FMK  │
   └──────────────────────────────┘                     └──────────────────────────────┘
                  ▼                                                    ▼
       [ Shared Folder Master Key (FMK) & Identical Folder ID Established in <30s ]
```

#### Action 3.1: Share the folder on Mac (Right Pane):
You can run `ferry share` inside the directory or pass the folder path:

```bash
# If already inside /tmp/ferry-sync-demo:
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry share

# Or with explicit path:
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry share /tmp/ferry-sync-demo
```
Ferry runs a secret scan, prints an ASCII QR code, and outputs a 6-character pairing code (such as `7KQ4-2M`).

#### Action 3.2: Join the folder on Arch Linux (Left Pane):
On Arch Linux, you do not need to run `ferry init` beforehand. `ferry join` automatically provisions the folder, sets up the database, and adopts the shared encryption key.

```bash
# Option A: In-directory (inside /tmp/ferry-sync-demo):
cd /tmp/ferry-sync-demo
~/.cargo/bin/ferry join 7KQ4-2M

# Option B: Explicit path:
~/.cargo/bin/ferry join 7KQ4-2M /tmp/ferry-sync-demo
```
Arch Linux confirms the join and adopts the folder identity immediately.

#### Action 3.3: Verify Identical Folder IDs:
Check status on both machines:
```bash
# On Mac:
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry status /tmp/ferry-sync-demo

# On Arch:
~/.cargo/bin/ferry status /tmp/ferry-sync-demo
```
Both machines report the exact same `folder_id`.

---

### Step 4: Start Background Sync Daemons

Turn on peer-to-peer synchronization between both devices.

#### Run on Mac (Right Pane):
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry daemon --listen 0.0.0.0:44001 /tmp/ferry-sync-demo
```

#### Run on Arch Linux (Left Pane):
Connect Arch Linux to your Mac Tailscale IP (`100.91.38.24`):
```bash
~/.cargo/bin/ferry daemon --peer-url 100.91.38.24:44001 /tmp/ferry-sync-demo
```
Both daemons confirm peer handshake and enter active synchronization.

---

### Step 5: Verify Live File Synchronization

Open new terminal splits to test file propagation.

#### Test 5.1: Create a file on Mac and verify on Arch:
```bash
# On Mac:
echo "Hello from MacBook Air!" > /tmp/ferry-sync-demo/hello.txt

# On Arch:
cat /tmp/ferry-sync-demo/hello.txt
```
The file appears immediately on Arch Linux.

#### Test 5.2: Create nested source code on Arch and verify on Mac:
```bash
# On Arch:
mkdir -p /tmp/ferry-sync-demo/backend/src
echo 'pub fn add(a: i32, b: i32) -> i32 { a + b }' > /tmp/ferry-sync-demo/backend/src/lib.rs

# On Mac:
cat /tmp/ferry-sync-demo/backend/src/lib.rs
```
The Rust source file appears immediately on Mac.

#### Test 5.3: Sync large binary payload:
```bash
# On Mac (generate 5MB binary):
dd if=/dev/urandom of=/tmp/ferry-sync-demo/large_asset.bin bs=1M count=5

# On Arch (verify hash):
sha256sum /tmp/ferry-sync-demo/large_asset.bin
```

---

### Step 6: Test Desktop GUI and Native Folder Picker

Launch the desktop GUI on your Mac:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry ui --gui /tmp/ferry-sync-demo
```

#### What to observe and test:
1. An Obsidian Dark desktop window opens.
2. The Status Beacon at the top right glows **GREEN (SYNCED)**.
3. The peer table lists your Arch Linux laptop as online.
4. Click **Select Folder** at the top right. A native OS directory selection dialog opens. Choosing a folder registers it in Ferry.
5. Click **Pin Session**. Enter `backend/**` and confirm. The beacon turns **PURPLE (HOLDING)**. Remote inbound writes to `backend/` are held while you edit.
6. Click **Pair Device** to display the pairing code modal and ASCII QR code.

---

### Step 7: Test Terminal TUI and In-TUI File Explorer

Launch the retro terminal dashboard on Arch Linux or Mac:
```bash
# On Arch Linux:
~/.cargo/bin/ferry tui /tmp/ferry-sync-demo

# Or on Mac:
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry tui /tmp/ferry-sync-demo
```

```
┌ Ferry Sync Engine ────────────────────────────────────── [ SYNCED ] ┐
│ Folder: /tmp/ferry-sync-demo                                         │
│ Manifest: e3b0c44298fc...         Peers Connected: 1/1               │
├────────────────────────────┬─────────────────────────────────────────┤
│ Storage & Transfer         │ Connected Fleet Peers                   │
│ Files: 4 (5.0 MB)          │ • arch-laptop [online] (synced 1s ago)  │
│ Transfer: Idle             │                                         │
├────────────────────────────┴─────────────────────────────────────────┤
│ Recent Activity Log                                                  │
│ [18:50:12] [INFO] Ingested chunk tree from peer (large_asset.bin)    │
│ [18:50:13] [INFO] Materialized 4 files to disk                       │
└──────────────────────────────────────────────────────────────────────┘
 [O] Open/Pick  [P] Pin  [R] Rescan  [C] Conflicts  [Q] Quit
```

#### Keyboard actions to test:
- Press **`O`**. Opens the interactive in-terminal Folder Picker modal.
  - Use **`Up`/`Down`** arrows to browse directory items.
  - Type characters for real-time substring filtering.
  - Press **`Enter`** to descend into a folder.
  - Press **`Space`** to select the active folder. Already synced directories are marked with an `already synced` badge.
  - Press **`Esc`** to dismiss the picker modal.
- Press **`P`**. Toggles Session Pinning. The header updates to `[ PINNED ]`.
- Press **`R`**. Triggers an immediate rescan of the workspace.
- Press **`C`**. Opens the Conflict inspection modal.
- Press **`Q`**. Quits the TUI and restores terminal state.

---

### Step 8: Test Web Dashboard with Live SSE Updates

Launch the web interface on Mac:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry ui --web --port 8080 /tmp/ferry-sync-demo
```
Your browser opens automatically to `http://127.0.0.1:8080/?token=...`.

#### What to test:
1. Verify live storage statistics and connected peer status.
2. Test the Web Folder Browser to explore directories from your browser.
3. Write a new file to `/tmp/ferry-sync-demo/test.txt` in your terminal. Watch the web dashboard update automatically without page refreshes through Server-Sent Events.
4. Open `http://127.0.0.1:8080/api/status` in an incognito window without the token. The server returns `403 Forbidden` because API endpoints require authentication tokens.

---

### Step 9: Test Conflict Handling and Quarantine

Verify how Ferry handles concurrent edits safely without data loss.

1. Stop both daemons with `Ctrl+C`.
2. Edit the same file on Mac:
   ```bash
   echo "Mac version" > /tmp/ferry-sync-demo/conflict_test.txt
   ```
3. Edit the same file on Arch:
   ```bash
   echo "Arch version" > /tmp/ferry-sync-demo/conflict_test.txt
   ```
4. Restart both daemons.
5. Ferry deterministically selects the winner based on timestamp. The losing version is quarantined under:
   `/tmp/ferry-sync-demo/conflict_test.txt.ferry-conflict.<device-id>-<timestamp>`
6. Inspect conflicts via CLI:
   ```bash
   /Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry conflicts list /tmp/ferry-sync-demo
   ```
7. Open the GUI or TUI. The status beacon turns **RED (CONFLICT)** and displays the conflict entry.

---

### Step 10: Test Secret Risk Gating

Ferry scans for leaked API keys, tokens, and private keys before sharing.

1. Create a dummy secret file:
   ```bash
   echo "AWS_SECRET_ACCESS_KEY=AKIAIOSFODNN7EXAMPLEKEY" > /tmp/ferry-sync-demo/.env
   ```
2. Try sharing the folder:
   ```bash
   /Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry share /tmp/ferry-sync-demo
   ```
3. Ferry refuses to emit a pairing code and prints a redacted warning of the detected secret.
4. Add the file to ignore rules or pass `--i-know` to proceed:
   ```bash
   /Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry ignore add '.env' /tmp/ferry-sync-demo
   ```

---

### Step 11: Clean Up

Stop background processes and remove temporary test files:

```bash
# On Mac:
pkill -f "ferry daemon"
pkill -f "ferry ui"
rm -rf /tmp/ferry-sync-demo

# On Arch:
pkill -f "ferry daemon"
pkill -f "ferry ui"
rm -rf /tmp/ferry-sync-demo
```

---

## 5. Command Quick Reference

| Command | Environment | Description |
| :--- | :--- | :--- |
| `ferry init [folder]` | Mac and Arch | Initializes cryptographic folder identity and store (defaults to current dir) |
| `ferry share [folder]` | Mac (Sharer) | Scans for secrets and generates 30s pairing code and ASCII QR |
| `ferry join <code> [dir]` | Arch (Joiner) | Adopts folder, auto-provisions store, and establishes pairing |
| `ferry daemon --listen 0.0.0.0:44001 [folder]` | Mac | Runs sync server listening for peer connections |
| `ferry daemon --peer-url <IP:44001> [folder]` | Arch | Connects to peer listener daemon over Tailscale |
| `ferry ui --gui [folder]` | Mac | Launches native desktop window (egui) with OS folder picker |
| `ferry tui [folder]` | Mac or Arch | Launches retro terminal dashboard (ratatui) with in-TUI picker |
| `ferry ui --web --port 8080 [folder]` | Mac or Arch | Launches browser dashboard with live SSE updates and web picker |
| `ferry pin start --paths '<glob>' [folder]` | Mac or Arch | Holds remote writes during active editing |
| `ferry pin stop [folder]` | Mac or Arch | Releases session hold |
| `ferry conflicts list [folder]` | Mac or Arch | Lists quarantined conflict files |
| `ferry status [folder]` | Mac or Arch | Displays current engine status and transfer metrics |
