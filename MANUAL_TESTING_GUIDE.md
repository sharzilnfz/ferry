# The Complete Guide to Ferry: Architecture, Concepts and Manual Testing

Welcome to Ferry. This guide explains how Ferry works, where each component lives, and how to verify every feature with **two real devices**: your Mac and your Arch Linux laptop. Every command below was verified against the current build (macOS, Aug 2026).

> **One rule before you start:** pair with `ferry pair` / `ferry pair --accept`
> (the offer-file flow). Do **not** use `ferry share` + `ferry join <CODE>` for
> a two-device test — as of this build the short-code path pairs both devices
> but never tells the sharer about the joiner, so the sharer's daemon denies
> every handshake and nothing ever syncs (issue T-016 in
> `.scratch/v1/issues/`). Pairing succeeds; sync silently doesn't.

---

## 1. The Big Picture

Ferry is a fast, encrypted, peer-to-peer folder synchronization engine built for developers.

- **The Problem**. You write code on macOS, but compile and run tests on an Arch Linux laptop. Moving code with Git commits or third-party clouds adds latency and leaks secrets.
- **The Solution**. Ferry continuously watches a local workspace directory. When you edit a file, Ferry splits changes into content-addressed chunks, encrypts them with a shared folder key, and beams them over your private Tailscale network in milliseconds.

---

## 2. System Topology and Architecture

Ferry connects two physical machines directly over Tailscale WireGuard tunnels.

```
┌────────────────────────────────────────────────────────┐   Tailscale WireGuard Mesh   ┌────────────────────────────────────────────────────────┐
│                      MACBOOK AIR                       │ ◄══════════════════════════► │                   ARCH LINUX LAPTOP                    │
│                                                        │                              │                                                        │
│ • IP: 100.91.38.24                                     │                              │ • IP: 100.122.159.26                                   │
│ • Code Repo: /Users/sharzilnafis/Projects/dumps/idea2  │                              │ • Code Repo: /home/sharzil/Projects/dumps/ferry        │
│ • Binary: ./target/debug/ferry                         │                              │ • Binary: ~/.cargo/bin/ferry                           │
│ • Herdr Pane: RIGHT PANE                               │                              │ • Herdr Pane: LEFT PANE (ssh sharzil@sharzilx)         │
│ • Test Folder: /tmp/ferry-sync-demo                    │                              │ • Test Folder: /tmp/ferry-sync-demo                    │
└────────────────────────────────────────────────────────┘                              └────────────────────────────────────────────────────────┘
```

### Herdr Terminal Setup
- **Left Pane**. Connected to your Arch Linux machine over SSH (`sharzil@sharzilx`).
- **Right Pane**. Connected to your local macOS shell.

Run the daemons in dedicated panes and stop them with `Ctrl+C` in that pane. Never `pkill -f ferry` — if a verification run ever leaves an orphan behind, find it by port with `lsof -nP -iTCP -sTCP:LISTEN | grep 44001` and kill that exact PID.

### Centralized Device Daemon and Folder Structure
Ferry runs a centralized device daemon per user.
- `$FERRY_HOME/daemon.sock`. IPC Unix domain socket used by CLI and frontends to communicate with the supervisor.
- `$FERRY_HOME/folders.toml`. Registry recording all synced folders, folder IDs, and root paths.
- `$FERRY_HOME/identity`. The device Ed25519 cryptographic keypair.

The device identity is what peers authorize against: each folder's `.ferry/config` records the device public keys allowed to hold the folder key. That is why the pairing step matters — pairing is how each side learns the other's key.

Inside each synced folder (e.g. `/tmp/ferry-sync-demo`), Ferry maintains a hidden `.ferry/` state directory:
- `.ferry/config`. Cryptographic header containing the encrypted Folder Master Key (FMK) plus one key-wrap entry per known device.
- `.ferry/settings.toml`. Folder-specific synchronization settings.
- `.ferry/store/`. Content-addressable chunk store.
- `.ferry/pin-state.json`. Active session hold and pinning status.
- `ferry.ignore`. The folder's ignore rules (gitignore syntax), next to `.ferry/` — not inside it.

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
3. **Web Dashboard (`ferry ui --web`)**. Browser interface built with axum. Delivers real-time updates via Server-Sent Events (SSE), directory exploration, and token-based URL authentication. The token is printed in the startup URL — there is no other way to retrieve it, so launch with the output visible.

Note: `ferry daemon --ui <ADDR>` is a *different*, older dashboard — loopback-only and **unauthenticated**. `ferry ui --web` is the token-gated one. Don't mix them up.

---

## 4. Step-by-Step Hands-On Tutorial

Follow these steps in order. Each step ends with something you can observe; don't move on until you've seen it.

---

### Step 1: Prepare the Test Environment

Create a clean demo workspace on both machines.

#### Run on Mac (Right Pane):
```bash
mkdir -p /tmp/ferry-sync-demo
```

#### Run on Arch Linux (Left Pane):
```bash
mkdir -p /tmp/ferry-sync-demo
```

---

### Build Workflow, Speed, & Disk Management

Before building, here are practical tips to keep compilation fast and disk usage low:

* **Debug vs. Release builds**:
  * **Day-to-day testing (Fastest)**: Run `cargo build -p ferry-cli`. The binary is at `./target/debug/ferry`. Compiling takes only a few seconds on incremental changes.
  * **Release benchmarking (Slower)**: Run `cargo build --release -p ferry-cli`. The binary is at `./target/release/ferry`. Uses full link-time optimization (LTO) and takes several minutes.
  * **Direct invocation**: Cargo can build and run in one step: `cargo run -p ferry-cli -- <command>`.
* **Compiler Caching (`sccache`)**:
  * Avoid recompiling dependencies from scratch by installing `sccache`:
    * Mac: `brew install sccache`
    * Arch Linux: `sudo pacman -S sccache`
    * Enable in shell: `echo 'export RUSTC_WRAPPER=sccache' >> ~/.zshrc` (or `~/.bashrc`)
* **Disk Cleanup**:
  * Rust build artifacts (`target/`) can grow to 20+ GB over time.
  * Run `cargo clean` to wipe all build artifacts and reclaim disk space immediately.
  * Run `cargo clean --release` to keep debug builds while freeing release artifacts.
* **Single-Machine Simulation (Fastest testing loop)**:
  * You can test multi-device synchronization entirely on your Mac without SSH or the Arch laptop by isolating daemon state with `FERRY_HOME`:
    ```bash
    # Simulated Device A
    export FERRY_HOME=/tmp/ferry-dev-a
    ./target/debug/ferry daemon start &
    ./target/debug/ferry pair /tmp/ferry-sync-demo

    # Simulated Device B
    export FERRY_HOME=/tmp/ferry-dev-b
    ./target/debug/ferry daemon start &
    ./target/debug/ferry pair --accept /tmp/ferry-sync-demo/.ferry/pair-offer.ferry-pair /tmp/ferry-sync-demo-b
    ```

---

### Step 2: Build and Initialize

#### Mac:
```bash
cd /Users/sharzilnafis/Projects/dumps/idea2
cargo build -p ferry-cli              # Fast debug build -> ./target/debug/ferry
# Or for release: cargo build --release -p ferry-cli -> ./target/release/ferry

./target/debug/ferry init /tmp/ferry-sync-demo
```

#### Arch Linux:
```bash
cd ~/Projects/dumps/ferry
cargo build -p ferry-cli              # Fast debug build -> ./target/debug/ferry

./target/debug/ferry init /tmp/ferry-sync-demo
```

Both options generate the `.ferry/` repository directory, cryptographic identity, and chunk store.

**Observe:** on either machine, `ferry status /tmp/ferry-sync-demo` now returns a real status line, and `test -f /tmp/ferry-sync-demo/.ferry/config` succeeds. Before init, the same status command fails with `code: not-a-folder` — that failure is the expected, healthy signal.

---

### Step 3: Pair the Two Devices (offer-file flow)

`ferry pair` writes an offer file, prints a short code + QR, **and keeps waiting** for the other side. `ferry pair --accept` on the other device consumes the offer and completes the key wrap — after this, *both* folders know *both* device keys. That last part is the whole point of this step.

#### Action 3.1: Share the folder on Mac (Right Pane):
```bash
./target/debug/ferry pair /tmp/ferry-sync-demo
```
Leave this running — it polls for the acceptor. Note the **Offer file** path it prints (default: `/tmp/ferry-sync-demo/.ferry/pair-offer.ferry-pair`) and the share code shown on both screens (they must match; that comparison is your defense against a moved file).

#### Action 3.2: Accept on Arch Linux (Left Pane):
Move the offer file however you move secrets (AirDrop, scp, USB):
```bash
scp mac:/tmp/ferry-sync-demo/.ferry/pair-offer.ferry-pair /tmp/
~/.cargo/bin/ferry pair --accept /tmp/pair-offer.ferry-pair /tmp/ferry-sync-demo
```
Arch confirms the join and adopts the folder identity. The Mac's `ferry pair` exits on its own once the accept lands.

**Observe:** run on both machines:
```bash
ferry status --json /tmp/ferry-sync-demo
```
Both report the **exact same `folder_id`**. Now check the key-wrap entries:
```bash
ferry status --json /tmp/ferry-sync-demo | grep -o device_id
```
Each side must know the other. If a later step's sync silently does nothing with `"peers":[]`, the pairing never completed on the sharer — re-run Step 3 rather than debugging the daemon.

---

### Step 4: Start Background Sync Daemons

Turn on peer-to-peer synchronization between both devices.

#### Run on Mac (Right Pane):
```bash
./target/debug/ferry daemon --listen 0.0.0.0:44001 /tmp/ferry-sync-demo
```

#### Run on Arch Linux (Left Pane):
Point Arch at your Mac's Tailscale IP:
```bash
~/.cargo/bin/ferry daemon --peer-url 100.91.38.24:44001 --interval-secs 1 /tmp/ferry-sync-demo
```
The dialer (Arch) drives an exchange round every `--interval-secs` seconds; the listener (Mac) serves sessions.

**Observe:** on Arch:
```bash
~/.cargo/bin/ferry status --json /tmp/ferry-sync-demo
```
The `peers` array must be non-empty and carry a `last_agreed_manifest_id` (that key lives *inside* each `peers[]` entry, not at the top level). An empty `peers:[]` with matching folder IDs means the daemons can't talk — check Step 3's key wrap and that port 44001 is reachable over Tailscale.

> **Sequencing rule:** don't write test files until you've seen `last_agreed_manifest_id` appear. Edits made before the first agreement settles are ambiguous.

---

### Step 5: Verify Live File Synchronization

#### Test 5.1: Create a file on Mac and verify on Arch:
```bash
# On Mac:
echo "Hello from MacBook Air!" > /tmp/ferry-sync-demo/hello.txt

# On Arch:
cat /tmp/ferry-sync-demo/hello.txt
```
The file appears within a second or two. Verify **byte-for-byte**, not just existence:
```bash
cksum /tmp/ferry-sync-demo/hello.txt   # run on BOTH; outputs must match
```

#### Test 5.2: Create nested source code on Arch and verify on Mac:
```bash
# On Arch:
mkdir -p /tmp/ferry-sync-demo/backend/src
echo 'pub fn add(a: i32, b: i32) -> i32 { a + b }' > /tmp/ferry-sync-demo/backend/src/lib.rs

# On Mac:
cat /tmp/ferry-sync-demo/backend/src/lib.rs
```

#### Test 5.3: Sync a large binary payload and an exec bit:
```bash
# On Mac:
dd if=/dev/urandom of=/tmp/ferry-sync-demo/large_asset.bin bs=1M count=5
printf '#!/bin/sh\necho ferry\n' > /tmp/ferry-sync-demo/run.sh && chmod 755 /tmp/ferry-sync-demo/run.sh

# On Arch:
sha256sum /tmp/ferry-sync-demo/large_asset.bin   # compare against Mac's
test -x /tmp/ferry-sync-demo/run.sh && echo "exec bit survived"
```

#### Test 5.4: Mutate an already-synced file:
```bash
# On Mac (file already exists on both sides):
echo "late edit" >> /tmp/ferry-sync-demo/hello.txt

# On Arch:
tail -n 1 /tmp/ferry-sync-demo/hello.txt   # must print "late edit"
```
Mutations of existing trees are a different code path from creations — always test one.

#### Test 5.5: Confirm `.env` never leaves the machine:
```bash
# On Mac:
echo "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE" > /tmp/ferry-sync-demo/.env
sleep 3

# On Arch:
ls /tmp/ferry-sync-demo/.env   # must NOT exist
```
`.env`-class files are excluded by default (`ferry ignore --list` shows the rule layers). This is selective sync doing its job — the absence on Arch is the passing result.

---

### Step 6: Test Session Pinning

Pinning declares one device the active writer: competing remote edits to the pinned paths are **held** instead of racing your tree.

```bash
# On Mac:
./target/debug/ferry pin start --paths 'backend/**' --hours 8 /tmp/ferry-sync-demo
test -f /tmp/ferry-sync-demo/.ferry/pin-state.json && echo "pin recorded"

# On Arch (while Mac is pinned):
echo "remote edit" > /tmp/ferry-sync-demo/backend/src/remote.txt

# On Mac:
./target/debug/ferry pin status /tmp/ferry-sync-demo   # shows the held set
ls /tmp/ferry-sync-demo/backend/src/remote.txt           # NOT there yet — it's held

# Release and reconcile:
./target/debug/ferry pin release /tmp/ferry-sync-demo  # three-way: winner live, loser quarantined
ls /tmp/ferry-sync-demo/backend/src/                     # remote.txt lands (or a conflict file appears)
./target/debug/ferry pin stop /tmp/ferry-sync-demo
```

---

### Step 7: Test Desktop GUI and Native Folder Picker

Launch the desktop GUI on your Mac:
```bash
./target/debug/ferry ui --gui /tmp/ferry-sync-demo
```

#### What to observe and test:
1. An Obsidian Dark desktop window opens.
2. The Status Beacon at the top right glows **GREEN (SYNCED)**.
3. The peer table lists your Arch Linux laptop as online.
4. Click **Select Folder** at the top right. A native OS directory selection dialog opens. Choosing a folder registers it in Ferry.
5. Click **Pin Session**. Enter `backend/**` and confirm. The beacon turns **PURPLE (HOLDING)**. Remote inbound writes to `backend/` are held while you edit.
6. Click **Pair Device** to display the pairing code modal and ASCII QR code.

These are interactive — there is no headless driver for the GUI, so this step is manual by design.

---

### Step 8: Test Terminal TUI and In-TUI File Explorer

Launch the retro terminal dashboard:
```bash
# On Arch Linux:
~/.cargo/bin/ferry tui /tmp/ferry-sync-demo
```

```
┌ Ferry Sync Engine ────────────────────────────────────── [ SYNCED ] ┐
│ Folder: /tmp/ferry-sync-demo                                         │
│ Manifest: e3b0c44298fc...         Peers Connected: 1/1               │
├────────────────────────────┬─────────────────────────────────────────┤
│ Storage & Transfer         │ Connected Fleet Peers                   │
│ Files: 4 (5.0 MB)          │ • arch-laptop [online] (synced 1s ago)  │
│ Transfer: Idle             │                                         │
├────────────────────────────┴────────────────────────────────────────┤
│ Recent Activity Log                                                  │
│ [18:50:12] [INFO] Ingested chunk tree from peer (large_asset.bin)    │
│ [18:50:13] [INFO] Materialized 4 files to disk                       │
└──────────────────────────────────────────────────────────────────────┘
 [O] Open/Pick  [P] Pin  [R] Rescan  [C] Conflicts  [Q] Quit
```

#### Keyboard actions to test:
- **`O`** opens the in-terminal Folder Picker modal: arrows browse, typing filters, `Enter` descends, `Space` selects (already-synced dirs show a badge), `Esc` dismisses.
- **`P`** toggles Session Pinning — the header flips to `[ PINNED ]`.
- **`R`** triggers an immediate rescan.
- **`C`** opens the Conflict inspection modal.
- **`Q`** quits and restores the terminal.

---

### Step 9: Test Web Dashboard with Token Auth and Live Updates

Launch the web interface on Mac:
```bash
./target/debug/ferry ui --web --port 8080 /tmp/ferry-sync-demo
```
It prints a URL of the form `http://127.0.0.1:8080/?token=<hex>` and opens your browser. The token lives **only in that printed URL** — don't lose it. (Use `--no-open` if you want to copy the URL manually.)

#### What to test:
1. With the token in the URL: live storage statistics and connected peer status render.
2. **Write a new file from your terminal** (`echo test > /tmp/ferry-sync-demo/test.txt`) and watch the dashboard update without a refresh (SSE).
3. **Token gate:** in a terminal:
   ```bash
   curl -s -o /dev/null -w '%{http_code}\n' "http://127.0.0.1:8080/api/status"            # 401/403 — blocked
   curl -s -o /dev/null -w '%{http_code}\n' "http://127.0.0.1:8080/api/status?token=WRONG" # 401/403 — blocked
   curl -s -w '\n%{http_code}\n' "http://127.0.0.1:8080/api/status?token=<real-token>"     # 200 + JSON
   ```
   Static assets (`/`, `/style.css`, `/app.js`) are intentionally tokenless — only API endpoints are gated.

---

### Step 10: Test Conflict Handling and Quarantine

Ferry never merges. Concurrent edits are resolved deterministically (timestamp-based winner) and the loser is **quarantined**, never destroyed.

1. **Stop both daemons** (`Ctrl+C` in their panes) — with daemons running, the second "concurrent" write usually just arrives later and wins cleanly.
2. Edit the same file on Mac:
   ```bash
   echo "Mac version" > /tmp/ferry-sync-demo/conflict_test.txt
   ```
3. Edit the same file on Arch:
   ```bash
   echo "Arch version" > /tmp/ferry-sync-demo/conflict_test.txt
   ```
4. Restart both daemons (Step 4 commands).
5. Wait a few seconds for an exchange round, then inspect:
   ```bash
   # On whichever side lost (the timestamp decides; you can't predict it):
   ls /tmp/ferry-sync-demo/*.ferry-conflict.*
   cat /tmp/ferry-sync-demo/*.ferry-conflict.*     # must be the loser's FULL content
   ferry conflicts list --json /tmp/ferry-sync-demo   # structured report, oldest first
   ```
6. The winner's content sits at the original path on the other machine. Open the GUI or TUI — the status beacon turns **RED (CONFLICT)**.

Nothing was merged and nothing was lost: that's the whole assertion.

---

### Step 11: Test Secret Risk Gating

Ferry scans for leaked API keys, tokens, and private keys before any pairing payload leaves the machine.

1. `.env` is excluded by default, so opt it back in to exercise the gate:
   ```bash
   ./target/debug/ferry ignore '!.env' /tmp/ferry-sync-demo
   ```
2. Try to share:
   ```bash
   ./target/debug/ferry share /tmp/ferry-sync-demo
   ```
3. Ferry **refuses** (non-zero exit), names the finding class (`aws-access-key`), and prints a **redacted** preview — the actual key bytes must never appear in the output. Check nothing was emitted:
   ```bash
   test ! -f /tmp/ferry-sync-demo/.ferry/pair-offer.ferry-pair && echo "nothing leaked"
   ```
4. Restore the default (there is no `ignore remove`; edit the file):
   ```bash
   grep -v '^!\.env$' /tmp/ferry-sync-demo/ferry.ignore > /tmp/x && mv /tmp/x /tmp/ferry-sync-demo/ferry.ignore
   ```

---

### Step 12: Clean Up

Stop background processes and remove temporary test files:

```bash
# On BOTH machines: Ctrl+C the daemon/UI processes in their panes.
# Then verify nothing is left behind:
lsof -nP -iTCP -sTCP:LISTEN | grep -E '44001|8080'   # must print nothing

rm -rf /tmp/ferry-sync-demo /tmp/pair-offer.ferry-pair
```

If you must kill remotely: find the exact PID via `lsof` (or `pgrep -f "ferry daemon"` to *look*, then kill the specific PID). Never `pkill -f ferry` blind — it kills every ferry process including unrelated ones.

---

## 5. Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `status` says `not-a-folder` | No `.ferry/` in that directory | `ferry init` there, or pass the right path |
| Sync silently does nothing; `peers:[]` on joiner | Pairing never completed on the sharer (allow-list denies the joiner) — the share/join code path does this | Re-pair with `ferry pair` / `pair --accept` (Step 3) |
| `peers:[]` but pairing completed | Daemons can't reach each other | Confirm Tailscale IP reachable, port 44001 open, listener actually bound (`lsof`) |
| File exists but `cksum` differs | Exchange still in flight | Wait one `--interval-secs` round; re-check |
| Status has no `last_agreed_manifest_id` anywhere | First agreement not yet settled | Wait; don't write test files until it appears |
| Web dashboard 403 on everything | Missing or wrong token in URL | Copy the full printed URL; token is query-param `?token=` |
| Port 44001 "address already in use" | Stale daemon from a previous session | `lsof -nP -iTCP -sTCP:LISTEN \| grep 44001`, kill that exact PID |
| `release/ferry: No such file or directory` | Executing relative path without `target/` prefix or built in debug mode | Run `./target/debug/ferry` (or `./target/release/ferry` if compiled with `--release`) |
| `target/` directory taking 20+ GB storage | Accumulation of intermediate compiler artifacts | Run `cargo clean` (or `cargo clean --release`) |
| Orphaned ferry processes after cleanup | Subshell backgrounding records the wrapper PID, not the daemon's | Find by port via `lsof`; kill the specific PID |

Universal doctor command, run on either machine whenever anything looks off:
```bash
ferry status --json /tmp/ferry-sync-demo   # JSON with no "code" field = folder healthy
```

---

## 6. Command Quick Reference

| Command | Environment | Description |
| :--- | :--- | :--- |
| `ferry init [folder]` | Mac and Arch | Initializes cryptographic folder identity and store (defaults to current dir) |
| `ferry pair [folder]` | Sharer | Writes offer file, prints code + QR, waits for the acceptor, completes the key wrap on both sides |
| `ferry pair --accept <file> [dir]` | Joiner | Consumes the offer file and adopts the folder |
| `ferry share [folder]` | Sharer | Secret-scan + short code + QR. Pairs but does **not** complete the sharer's key wrap — no live sync until T-016 is fixed |
| `ferry join <code> [dest]` | Joiner | Adopts a folder via short code (same T-016 caveat) |
| `ferry daemon --listen 0.0.0.0:44001 [folder]` | Mac | Runs sync server listening for peer connections |
| `ferry daemon --peer-url <IP>:44001 [folder]` | Arch | Dials the listener and drives exchange rounds every `--interval-secs` |
| `ferry sync --peer-url <IP>:44001 [folder]` | Either | One-shot exchange; exit 0 = converged |
| `ferry ui --gui [folder]` | Mac | Native desktop window (egui) with OS folder picker |
| `ferry tui [folder]` | Mac or Arch | Retro terminal dashboard (ratatui) with in-TUI picker |
| `ferry ui --web --port 8080 [folder]` | Mac or Arch | Browser dashboard, token-gated, live SSE updates |
| `ferry pin start --paths '<glob>' [--hours N] [folder]` | Mac or Arch | Holds remote writes during active editing |
| `ferry pin status [folder]` | Mac or Arch | Shows pin state and the full held set |
| `ferry pin release [folder]` | Mac or Arch | Reconciles held changes: winner live, loser quarantined |
| `ferry pin stop [folder]` | Mac or Arch | Ends the session without reconciling |
| `ferry conflicts list [--json] [folder]` | Mac or Arch | Structured conflict report, oldest first |
| `ferry ignore [--list] [pattern] [folder]` | Mac or Arch | Append rules, apply presets (`claude`, `opencode`), show layers |
| `ferry status [--json] [folder]` | Mac or Arch | The doctor: peers, agreement, conflicts, pending work |
