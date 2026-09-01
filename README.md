# Ferry

[![ci](https://github.com/sharzilnfz/ferry/actions/workflows/ci.yml/badge.svg)](https://github.com/sharzilnfz/ferry/actions/workflows/ci.yml)
[![license](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

**Secure full-file sync for developers and their agents.**

Your entire project directory stays identical on every machine you touch.
This includes everything git refuses to carry: `node_modules`, `.env`, build caches, agent state, datasets.
Content is end-to-end encrypted, peer-to-peer when possible, and relayed when necessary.
Git manages source history; Ferry synchronizes everything else.

## Why Ferry exists

Git syncs tracked source code.
Nothing good syncs everything else:

- **Cloud drives** choke on symlinks, corrupt `node_modules`, and inspect code on their servers.
- **General-purpose sync tools** perform poorly on dependency trees and lack agent awareness.
- **rsync and scp** are manual, one-directional, and stateless.
- **AI coding agents** churn through thousands of files overnight. Safe multi-device coordination is necessary.

## How it works

1. **Sync stores, not trees.** Every machine maintains a content-addressed store of encrypted chunk packs and signed manifests. Local trees materialize on demand.
2. **End-to-end encrypted by default.** Content is encrypted with ChaCha20-Poly1305 before leaving the device. Devices pair over shortcode rendezvous or out-of-band envelopes.
3. **Peer-to-peer first.** Uses direct QUIC connections with mDNS local discovery and blind relay fallback.
4. **Conflicts quarantine, never merge.** Concurrent edits produce deterministic conflict files (`*.ferry-conflict.*`) and structured logs in `.ferry/conflicts.jsonl`.
5. **Agentic workflows.** Supports session pinning (`ferry pin start`) to hold remote writes during intense edits, plus layered ignore rules and secret-scan gating.

## Quick start

Requirements: Rust 1.75+ (stable). macOS, Linux, and Windows are supported.

```sh
git clone https://github.com/sharzilnfz/ferry.git
cd ferry
cargo build --release
```

### Two-device synchronization

```sh
# Device A: initialize and share
ferry init
ferry share            # displays a 6-character short code and ASCII QR

# Device B: join over the network
ferry join <CODE>

# Both devices: background daemons sync automatically
ferry status           # check peers, agreements, and pending work
ferry status --json    # machine-readable status
```

### Frontends and control

```sh
ferry ui --gui         # native desktop application (Obsidian Dark styling)
ferry tui              # retro terminal dashboard with live meters
ferry ui --web         # browser dashboard with real-time SSE stream
ferry ui token         # retrieve active web session authentication token
```

### Session pinning and maintenance

```sh
ferry pin start --paths 'backend/**'  # hold competing remote edits
ferry pin status                      # inspect held remote edits
ferry pin release                     # reconcile held edits into tree
ferry conflicts list                  # list quarantined conflict records
ferry store gc --dry-run              # report unreferenced store packfiles
```

## Architecture

The workspace is structured into specialized crates:

| Crate | Responsibility |
|---|---|
| `ferry-cli` | CLI entry point: commands, parsing, and auto-bootstrap supervisor |
| `ferry-crypto` | Device identity, key derivation, and ChaCha20-Poly1305 sealing |
| `ferry-daemon` | Central supervisor, device daemon, and web dashboard backend |
| `ferry-folder` | Folder lifecycle, cryptographic header storage, and pairing rituals |
| `ferry-gui` | Native desktop application with Obsidian Dark styling |
| `ferry-ignore` | Layered ignore rules, secret pattern scanner, and presets |
| `ferry-ipc` | Universal IPC transport, framing, and AutoBackend client fallback |
| `ferry-iroh` | Direct QUIC transport, UDP hole-punching, and blind relay fallbacks |
| `ferry-materialize` | Guarded atomic file applier and permission preservation |
| `ferry-platform` | Cross-platform paths, locking, case-folding, and time utilities |
| `ferry-proto` | Binary wire protocol framing, streams, and envelope structures |
| `ferry-relay` | Blind relay server for NAT traversal fallbacks |
| `ferry-rendezvous` | Network discovery, socket framing, and shortcode rendezvous server |
| `ferry-scan` | Working tree scanner and change engine with notify event watchers |
| `ferry-store` | Content-addressable chunk store, Rabin CDC, packfiles, and GC |
| `ferry-sync` | P2P exchange engine, continuous sync loops, and transport adapters |
| `ferry-sync-engine` | 3-way reconciliation, session pinning, and conflict logging |
| `ferry-tui` | Retro terminal dashboard with live meters and in-TUI browser |

Every applied change goes through an atomic pipeline: verify local state, stage temp files, re-hash chunk regions, and atomically rename to destination.

## Platform support

| Platform | CI | Notes |
|---|---|---|
| Linux (ubuntu-24.04) | ✅ | Reference platform |
| macOS (macos-14) | ✅ | Full fidelity including nanosecond timestamps |
| Windows (windows-2022) | ✅ | Executable bits carried in manifests; symlinks require Developer Mode |

## Documentation

- [`docs/manual-testing-guide.md`](docs/manual-testing-guide.md): complete dual-device live verification guide
- [`SPEC.md`](SPEC.md): specification and milestone definitions
- [`CONTEXT.md`](CONTEXT.md): glossary of domain terms
- [`docs/adr/`](docs/adr/): architecture decision records (ADR-0001 through ADR-0008)
- [`docs/cli-json.md`](docs/cli-json.md): CLI `--json` schema contract
- [`research/use-cases.md`](research/use-cases.md): developer and agent workflow research
- [`research/landscape.md`](research/landscape.md): comparative analysis with existing tools

## Development

```sh
cargo test --workspace          # run workspace test suite
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/quickstart-e2e.sh  # two-device end-to-end acceptance test
```

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
