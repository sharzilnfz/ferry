# Quickstart: two devices syncing in under five minutes

Ferry syncs developer directories between your machines, end-to-end
encrypted. This page walks the whole ritual with nothing but `--help`. It
takes about three minutes.

> **Honesty note (v0).** Today's transport is plain TCP between two
> addresses you give it — perfect for a laptop and a desktop on the same
> network (or two terminals on one machine). True peer-to-peer QUIC with
> NAT traversal and relays lands next (tickets T-009/T-014); the commands
> below will not change.

## 0. Install

From this repository:

```sh
cargo install --path crates/ferry-cli
ferry --version
```

(One-line curl|sh installer and Homebrew formula are planned; ticket T-013
notes them as later install paths.)

## 1. Initialize on device A

```sh
cd ~/my-project        # any directory you want synced
ferry init
```

This creates your **device identity** (`~/.ferry/`, or `$FERRY_HOME` if you
set it), an encrypted store under `my-project/.ferry/`, and a starter
`ferry.ignore`. Built-in defaults keep `.env` files and `node_modules/` out
of sync until you opt them in — see `ferry ignore --list`.

Drop a couple of files in the folder; they sync once another device pairs.

## 2. Pair device B

On **device A**:

```sh
ferry pair
```

You get a short code (like `7KQ4-2MNP-…`), an ASCII QR, and an **offer
file**: `~/my-project/.ferry/pair-offer.ferry-pair`.

The payload file stands in for QR-camera transport across machines: move
the offer file to device B however you move secrets (AirDrop, scp, USB).
Possession of those bytes is the authorization — that is exactly what a
camera scan proves in person.

On **device B**, in the folder you want populated (create one first):

```sh
mkdir ~/my-project && cd ~/my-project
ferry pair --accept /path/to/pair-offer.ferry-pair
```

Device B writes its response beside the offer file, receives the sealed
folder key, and builds its own encrypted store. Move the response + grant
files back so device A's `ferry pair` completes (same out-of-band channel).

`ferry share <folder>` does the same thing but runs a secret scan first:
if anything credential-shaped would sync, you get a loud redacted warning
and must pass `--i-know` to proceed.

## 3. Start syncing

Pick a port. On **device A** (the listener):

```sh
ferry daemon --listen 0.0.0.0:44001
```

On **device B** (the dialer):

```sh
ferry daemon --peer-url 192.168.1.20:44001    # A's address
```

Watch B's folder fill up. Edit on either side; changes flow both ways,
including deletions. Conflicts never merge: the loser becomes
`path.ferry-conflict.<device>-<time>` next to the winner, plus an entry in
the report:

```sh
ferry conflicts list
ferry status
```

Prefer one-shot? `ferry sync --peer-url <addr>` exchanges until both sides
agree, prints a summary, exits 0 when converged (1 on timeout).

## What's real today vs what's coming

| Capability | Status |
|---|---|
| Identity + pairing ritual (offer/response MAC, wrapped FMK) | real |
| Encrypted store per folder (ChaCha20-Poly1305 packs) | real |
| Watch + continuous sync over TCP | real |
| Three-way reconcile, conflict quarantine, JSONL report | real |
| Ignore rules, presets (claude/opencode), secret-scan gate | real |
| Transport discovery / NAT traversal / relay | T-009/T-014 |
| End-to-end wire encryption of exchange frames | T-014 (store at rest is already encrypted) |
| Recovery export (`~/.ferry` passphrase archive) | via ferry-crypto API; CLI command pending |

Two devices on ONE machine? Set `FERRY_HOME` per shell to simulate two
devices — that is exactly what `scripts/quickstart-e2e.sh` automates:

```sh
scripts/quickstart-e2e.sh     # two simulated devices, asserts convergence
```

## Losing everything

No accounts, no server: if you lose every paired device's
`~/.ferry/identity/device.key`, the folders' keys are gone with them.
Back up your identity directory.
