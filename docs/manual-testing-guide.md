# The Complete Beginner's Guide to Ferry: Architecture, Concepts & Manual UI Testing

Welcome! If this is your first time working with a Rust-based distributed system, don't worry. This guide explains **what everything is**, **where everything lives**, **why each command exists**, and **which terminal pane to run each command in**.

---

## 1. The Big Picture: What is Ferry?

Think of **Ferry** as your own private, encrypted **Dropbox + AirDrop** designed specifically for developers.

- **The Problem**: You write code on your **Mac**, but want to test or compile on your **Arch Linux laptop** without having to commit half-finished work to Git or upload sensitive files to a third-party cloud.
- **The Solution**: Ferry watches a designated folder on your Mac and the matching folder on your Arch Linux laptop. When you save a file on your Mac, Ferry encrypts the changed pieces and beams them directly to your Arch laptop over your private **Tailscale** mesh network in milliseconds.

---

## 2. System Topology: Where Does Everything Live?

You have two physical computers connected over Tailscale:

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

### Your Herdr Screen Setup:
- **Left Pane**: Connected to your **Arch Linux** machine via SSH (`sharzil@sharzilx`).
- **Right Pane**: Connected to your **Mac** local shell.

### What is inside a Synced Folder?
When you initialize a folder with Ferry (e.g. `/tmp/ferry-sync-demo`), Ferry creates a hidden `.ferry/` folder:
- `.ferry/identity`: Your device's cryptographic keypair (Ed25519 public/private keys).
- `.ferry/config.toml`: The settings, folder ID, and trusted peer list.
- `.ferry/store/`: The content-addressable chunk store (where encrypted file blocks live).
- `.ferry/ignore`: Files that Ferry should ignore (like `.git`, `node_modules/`, `target/`).
- `.ferry/daemon.sock`: An IPC Unix socket that Ferry's UIs use to talk to the running background engine.

---

## 3. The 3 UI Faces Explained

Ferry has one underlying engine (the **Daemon**) and three different frontends:

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

1. **Desktop GUI (`ferry ui --gui`)**:
   - Built with **`egui`** (a native Rust immediate-mode GUI framework).
   - Opens as a native macOS window on your Mac.
   - Shows a glowing status beacon (**Green** = Synced, **Blue** = Syncing, **Purple** = Pinned/Holding, **Red** = Conflict), peer fleet table, and clickable modals.
2. **Terminal TUI (`ferry tui`)**:
   - Built with **`ratatui`** (a Rust terminal UI library).
   - Runs directly inside your terminal screen (perfect for remote SSH on Arch or inside Herdr).
   - Shows live transfer progress bars, recent activity log, and supports keyboard hotkeys (`C`, `P`, `R`, `Q`).
3. **Web Dashboard (`ferry ui --web`)**:
   - Built with **`axum`** (a Rust async web server).
   - Serves an embedded single-page application with dark Obsidian glass styling.
   - Uses **Server-Sent Events (SSE)** so the webpage updates live without needing page refreshes.
   - Protected by a single-use cryptographically random token in the URL.

---

## 4. Step-by-Step Hands-On Tutorial

Let's test everything from scratch in 10 clear, easy steps.

---

### Step 1: Create the Test Folder on Both Machines

We will create a clean test folder `/tmp/ferry-sync-demo` on both your Mac and your Arch laptop.

#### 👉 Run on Mac [Right Pane]:
```bash
# 1. Go to your repo folder
cd /Users/sharzilnafis/Projects/dumps/idea2

# 2. Create the test folder
mkdir -p /tmp/ferry-sync-demo
```

#### 👉 Run on Arch Linux [Left Pane]:
```bash
mkdir -p /tmp/ferry-sync-demo
```

---

### Step 2: Initialize Ferry in the Test Folder

This command generates your device's cryptographic identity and local database.

#### 👉 Run on Mac [Right Pane]:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry init /tmp/ferry-sync-demo
```

#### 👉 Run on Arch Linux [Left Pane]:
```bash
~/.cargo/bin/ferry init /tmp/ferry-sync-demo
```

---

### Step 3: Pair the Mac and Arch Laptop (30-Second Seamless Onboarding)

Ferry provides a **zero-file 30-second rendezvous flow** with human-friendly 6-character pairing codes, secret scanning, and automated mutual onboarding.

```
       Mac (Sharer / Initiator)                           Arch Linux (Joiner / Acceptor)
   ┌──────────────────────────────┐                     ┌──────────────────────────────┐
   │ 1. `ferry share <folder>`    │                     │                              │
   │    • Scans for secrets       │                     │                              │
   │    • Emits short code & QR   │ ── Tell code ─────► │ 2. `ferry join <code> <dir>` │
   │      (e.g., 7KQ4-2M)         │    (over voice,     │    • Auto-provisions store   │
   │                              │     chat, or QR)    │    • Adopts folder ID & FMK  │
   └──────────────────────────────┘                     └──────────────────────────────┘
                  ▼                                                    ▼
       [ Shared Folder Master Key (FMK) & Identical Folder ID Established in <30s ]
```

#### Action 3.1: Share the folder on Mac [Right Pane]:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry share /tmp/ferry-sync-demo
```
*(Ferry runs an automatic credential/secret scan, emits a short 6-character share code like `7KQ4-2M`, displays an ASCII QR code, and registers the session.)*

#### Action 3.2: Join the folder on Arch Linux [Left Pane]:
Pass the code displayed on your Mac to `ferry join`:
```bash
~/.cargo/bin/ferry join 7KQ4-2M /tmp/ferry-sync-demo
```
*(Arch adopts the folder identity, imports the encrypted folder master key, initializes its local database, and prints `Joined <folder_id> at /tmp/ferry-sync-demo`!)*

#### Action 3.3: Verify Shared Identity:
Run this on both panes:
```bash
# On Mac:
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry status /tmp/ferry-sync-demo

# On Arch:
~/.cargo/bin/ferry status /tmp/ferry-sync-demo
```
👉 *Both computers will report the exact same `folder_id`!*

---

### Step 4: Start the Sync Daemons (Live P2P Connection)

Now we turn on continuous background synchronization.

#### 👉 Run on Mac [Right Pane]:
Tell Mac to listen on port `44001`:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry daemon --listen 0.0.0.0:44001 /tmp/ferry-sync-demo
```
*(You will see: `[INFO] Ferry daemon listening on 0.0.0.0:44001`)*

#### 👉 Run on Arch Linux [Left Pane]:
Tell Arch to connect to your Mac's Tailscale IP (`100.91.38.24`):
```bash
~/.cargo/bin/ferry daemon --peer-url 100.91.38.24:44001 /tmp/ferry-sync-demo
```
*(You will see: `[INFO] Connected to peer 100.91.38.24:44001`)*

**Congratulations! Your two machines are now live-syncing!**

---

### Step 5: Test Live File Syncing

Open another terminal or split pane on Mac and Arch to play with files:

#### Test 5.1: Create a file on Mac and watch it appear on Arch:
```bash
# On Mac:
echo "Hello from MacBook Air!" > /tmp/ferry-sync-demo/hello.txt

# On Arch:
cat /tmp/ferry-sync-demo/hello.txt
```
👉 *The file appears immediately on Arch!*

#### Test 5.2: Create a deep folder tree on Arch and watch it appear on Mac:
```bash
# On Arch:
mkdir -p /tmp/ferry-sync-demo/backend/src
echo 'pub fn add(a: i32, b: i32) -> i32 { a + b }' > /tmp/ferry-sync-demo/backend/src/lib.rs

# On Mac:
cat /tmp/ferry-sync-demo/backend/src/lib.rs
```
👉 *The Rust file appears on Mac!*

#### Test 5.3: Sync a large binary file:
```bash
# On Mac (create a 5MB random binary):
dd if=/dev/urandom of=/tmp/ferry-sync-demo/large_asset.bin bs=1M count=5

# On Arch (verify hash):
sha256sum /tmp/ferry-sync-demo/large_asset.bin
```

---

### Step 6: Test UI Face 1 — The Desktop GUI App (`egui`) & Folder Picker

In a new Mac terminal window or pane, launch the GUI:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry ui --gui /tmp/ferry-sync-demo
```

#### What to observe & test:
1. A sleek **Obsidian Dark** desktop window opens.
2. At the top right, see the **Status Beacon** glowing **GREEN (SYNCED)**.
3. In the **Connected Peers** table, see your Arch Linux laptop listed as **online**.
4. **Folder Picker & Add Folder**:
   - Click **Open Folder / Pick Directory** to trigger the interactive folder browser modal.
   - Navigate directories with instant breadcrumb path updates and single-click selection. Already synced folders are dimmed with an `already synced` badge.
5. Click **Pin Session** at the top:
   - Type `backend/**` in the path input.
   - Click Confirm.
   - Watch the Status Beacon turn **PURPLE (HOLDING)**! This declares to your Arch laptop that you are currently editing files under `backend/` and holds remote changes.
6. Click **Pair Device / Share** to view the 6-character short code and ASCII QR code generator modal.

---

### Step 7: Test UI Face 2 — The Interactive Terminal TUI (`ratatui`) & In-TUI Picker

On your Arch Linux machine (or in a Mac terminal), launch the TUI:

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

#### Interactive Keys to try:
- Press **`O`**: Opens the interactive in-TUI Folder Picker modal.
  - Use **`Up`/`Down`** arrows to browse directories.
  - Type characters for instant fuzzy/substring search filtering.
  - Press **`Enter`** to drill into a directory.
  - Press **`Space`** to select the highlighted directory (dimmed if already synced).
  - Press **`Esc`** to clear filter or exit the picker modal.
- Press **`P`**: Instantly toggles Session Pinning on/off. Notice the badge change to `[ PINNED ]`.
- Press **`R`**: Forces a fast full re-scan of the directory.
- Press **`C`**: Opens the Conflicts inspection popup.
- Press **`Esc`**: Closes the popup.
- Press **`Q`**: Safely quits the TUI and cleanly restores your terminal screen.

---

### Step 8: Test UI Face 3 — The Web Dashboard

Launch the web dashboard on Mac:
```bash
/Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry ui --web --port 8080 /tmp/ferry-sync-demo
```
*(Your Mac will automatically open Safari or Chrome to `http://127.0.0.1:8080/?token=...`)*

#### What to test:
1. Look at the live status numbers (Files, Scanned Bytes, Active Peers).
2. Test the Web Folder Browser / Picker interface to inspect directories without leaving the browser.
3. Create a new file in `/tmp/ferry-sync-demo/test.txt` from your terminal.
4. Look at your browser: **it updates automatically without you pressing Refresh!** (Powered by Server-Sent Events).
5. Try opening `http://127.0.0.1:8080/api/status` in an incognito window without the token — you will receive a `403 Forbidden` error because Ferry secures every API call.

---

### Step 9: How to Test Conflict Resolution

What happens if you edit the exact same line on both computers at the exact same second?

1. Stop the daemons on both sides (`Ctrl+C`).
2. On Mac:
   ```bash
   echo "Mac version of title" > /tmp/ferry-sync-demo/title.txt
   ```
3. On Arch:
   ```bash
   echo "Arch version of title" > /tmp/ferry-sync-demo/title.txt
   ```
4. Restart both daemons.
5. Ferry automatically resolves the conflict deterministically (latest timestamp wins), but **never deletes your data**. The losing version is safely quarantined as:
   `/tmp/ferry-sync-demo/title.txt.ferry-conflict.<device-id>-<timestamp>`
6. Run:
   ```bash
   /Users/sharzilnafis/Projects/dumps/idea2/target/release/ferry conflicts list /tmp/ferry-sync-demo
   ```
7. Open the TUI (`ferry tui`) or GUI (`ferry ui --gui`) and see the **RED CONFLICT** beacon and inspect the conflict report!

---

### Step 10: Clean Up

When you are done testing, stop the running daemons and delete the test sandbox:

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

| Command | Where to Run | What it Does |
| :--- | :--- | :--- |
| `ferry init <folder>` | Mac & Arch | Initializes cryptographic identity & store |
| `ferry share <folder>` | Mac (Sharer) | Scans for secrets and generates 30s pairing code & ASCII QR |
| `ferry join <code> <dir>` | Arch (Joiner) | Adopts folder, initializes store, and joins sync session |
| `ferry daemon --listen 0.0.0.0:44001 <folder>` | Mac | Runs background sync server listening for connections |
| `ferry daemon --peer-url <IP:44001> <folder>` | Arch | Connects to the listener daemon over Tailscale |
| `ferry ui --gui <folder>` | Mac | Launches native desktop window (egui) with folder picker |
| `ferry tui <folder>` | Mac or Arch | Launches retro terminal dashboard (ratatui) with in-TUI picker |
| `ferry ui --web --port 8080 <folder>` | Mac or Arch | Launches browser dashboard with live SSE updates & web picker |
| `ferry pin start --paths '<glob>' <folder>` | Mac or Arch | Holds remote writes for your active coding session |
| `ferry pin stop <folder>` | Mac or Arch | Releases session hold |
| `ferry conflicts list <folder>` | Mac or Arch | Displays quarantined conflict files |
| `ferry status <folder>` | Mac or Arch | Prints current engine state and sync statistics |
