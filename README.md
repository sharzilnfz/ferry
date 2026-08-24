# Ferry

[![ci](https://github.com/sharzilnfz/ferry/actions/workflows/ci.yml/badge.svg)](https://github.com/sharzilnfz/ferry/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

**Secure full-file sync for developers and their agents.**

Your entire project directory — identical on every machine you touch,
including everything git refuses to carry: `node_modules`, `.env`, build
caches, agent state, datasets. End-to-end encrypted, peer-to-peer when
possible, relayed when necessary. Git stays in charge of source history;
Ferry carries everything else.

## Why Ferry exists

Git syncs tracked source. Nothing good syncs everything else:

- **Cloud drives** choke on symlinked directories, corrupt `node_modules`,
  and read every byte of your code on their servers.
- **General-purpose sync tools** are close but crude for development
  workflows: weak conflict handling, painful performance on
  dependency-scale directories, no concept of agent workflows.
- **rsync/scp** are manual, one-directional, and stateless.
- **AI coding agents** raised the stakes. An agent can churn through
  thousands of files overnight on your desktop while you open the same
  project on a laptop across town. Until now there has been no safe story
  for that.

## How it works

1. **Sync stores, not trees.** Every machine keeps a content-addressed
   object store: hash-addressed blobs plus signed manifests. The network
   layer moves blobs and manifests; each machine materializes its own
   working tree locally. Hash equality makes delta detection free.
2. **End-to-end encrypted by default.** Content is encrypted before it
   leaves a device; no plaintext ever touches a relay or another disk.
   Devices pair through an explicit out-of-band payload, and `share`
   secret-scans the folder (flagging `.env`-class files) before you send
   anything.
3. **Peer-to-peer first.** Direct connections when possible, with a
   self-hostable relay as fallback. No vendor cloud is required, and a
   hosted relay would never be load-bearing.
4. **Conflicts quarantine, never merge.** Concurrent edits produce
   explicit conflict files next to the originals plus a structured report.
   Silent merges destroy trust; agents make concurrent writes common.
5. **Agentic workflows are a first-class target:** selective sync via
   layered ignore rules tuned for `node_modules`-scale directories,
   session pinning that declares one device the active writer for a
   while, and hydration of heavy dependencies from any peer that already
   has them.

## Quick start

Requirements: Rust 1.75+ (stable). macOS, Linux, and Windows are all
supported and tested in CI.

```sh
git clone https://github.com/sharzilnfz/ferry.git
cd ferry
cargo build --release
```

Two-device sync in five commands per side (or run
`scripts/quickstart-e2e.sh`, which scripts exactly this flow against two
local devices and asserts byte-for-byte convergence):

```sh
# Device A — create a folder and prepare it for sharing
ferry init
ferry share --out /media/usb/payload.ferrypair

# Device B — adopt the folder from the payload
ferry pair --from /media/usb/payload.ferrypair

# Both devices — watch and exchange continuously
ferry daemon
```

Inspect any folder at any time, human-readable or scripted:

```sh
ferry status              # peers, agreements, conflicts, pending work
ferry status --json       # stable JSON document (schema-tested in CI)
ferry pin start --hours 8 # declare this device the active writer
ferry conflicts           # structured conflict report
```

## Architecture

The workspace is a set of small, sharply separated crates:

| Crate | Responsibility |
|---|---|
| `ferry-cli` | The `ferry` binary: init, share, pair, status, pin, conflicts, ignore |
| `ferry-daemon` | Background watcher: scans folders and exchanges with peers |
| `ferry-sync` | Exchange sessions: handshake, encrypted transport, one-shot sync |
| `ferry-sync-engine` | Three-way reconciliation into explicit action plans |
| `ferry-materialize` | Guarded atomic applier: temp-write, verify, rename, restore metadata |
| `ferry-scan` | Working-tree scanner and change engine (native events + rescans) |
| `ferry-store` | Content-addressed store: chunker, blobs, manifests, snapshots |
| `ferry-proto` | Wire protocol: framing, hello/offer/agreement messages |
| `ferry-crypto` | Device identity, key handling, payload sealing |
| `ferry-relay` | Self-hostable store-and-forward relay |
| `ferry-iroh` | QUIC-based direct transports (iroh integration) |
| `ferry-ignore` | Layered ignore rules and presets |
| `ferry-pin` | Session pinning state and stale-holder detection |
| `ferry-platform` | Cross-platform file semantics: paths, case folding, links, time |

Every applied change goes through the same guarded pipeline: resolve
destinations, verify local losers, write to temp files, re-hash every
chunk region, then atomically rename — interrupted applies leave either
the old file or the new file, never half of either.

Cross-platform behavior is deliberate rather than incidental: manifests
carry mtime and exec-bit metadata, which Windows restores to the last
100ns-representable value and treats as advisory where NTFS has no
concept of the bit; symlink policy refuses anything that could escape the
sync root; reserved device names are refused loudly instead of silently
misbehaving.

## Platform support

| Platform | CI | Notes |
|---|---|---|
| Linux (ubuntu-24.04) | ✅ | Reference platform |
| macOS (macos-14) | ✅ | Full fidelity incl. nanosecond mtimes |
| Windows (windows-2022) | ✅ | Exec bits carried in manifests but not stored on disk; symlinks require Developer Mode or admin |

## Documentation

- [`SPEC.md`](SPEC.md) — v0 specification
- [`CONTEXT.md`](CONTEXT.md) — glossary of domain terms
- [`docs/adr/`](docs/adr/) — architecture decision records
- [`research/use-cases.md`](research/use-cases.md) — archetypes and pain points
- [`research/landscape.md`](research/landscape.md) — prior art worth borrowing

## Development

```sh
cargo test --workspace          # full suite, all platforms in CI
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/skeleton-e2e.sh    # walking-skeleton end-to-end
bash scripts/quickstart-e2e.sh  # zero-to-two-devices acceptance
bash scripts/adversarial-fixture.sh  # hostile-filename corpus
```

Status: v0.1.0, pre-release. The wire protocol and store format may still
change; do not build long-lived data on them yet.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.
