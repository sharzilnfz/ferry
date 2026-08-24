//! `ferry-sync` CLI: run one M0 daemon over iroh (default) or loopback TCP,
//! or generate a folder polynomial.
//!
//! Daemon pairs (iroh mode, the ADR-0003 path):
//!
//! ```text
//! # Node A (listener): announces its public endpoint id
//! ferry-sync daemon --role listen \
//!     --store /tmp/node-a/store --tree /tmp/node-a/tree --tag node-a --poly <hex16> \
//!     [--relay http://relay.example:3340] [--discovery mdns]
//! # -> prints `ENDPOINT <hex64>` once the QUIC endpoint is up
//!
//! # Node B (connector): dials A BY PUBLIC KEY
//! ferry-sync daemon --role connect --peer <A's hex64> \
//!     --store /tmp/node-b/store --tree /tmp/node-b/tree --tag node-b --poly <hex16> \
//!     [--relay http://relay.example:3340] ...
//! ```
//!
//! TCP mode (`--transport tcp`) keeps the M0 localhost shape: `--addr`
//! HOST:PORT means an actual socket again.
//!
//! Both print machine-greppable `STATE root=<hex> agreed=<hex|none>` lines;
//! the connector drives sessions, the listener serves them and relies on
//! the peer's opportunistic dials to discover its changes.
//!
//! Status lines go to stdout; errors to stderr.

use std::process::ExitCode;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::SeedableRng;

use ferry_crypto::identity as crypto_identity;
use ferry_store::format::unhex;
use ferry_sync::{EngineConfig, SyncEngine};

/// Fixed route-key label for iroh-mode dialing: `--addr` is meaningless on
/// the wire there, so the connector uses one reserved alias registered to
/// `--peer`'s public key before the engine starts.
const IROH_DIAL_ALIAS: &str = "127.0.0.1:65534";
/// Same for listeners: port 0 makes the transport mint a unique alias.
const IROH_BIND_ALIAS: &str = "127.0.0.1:0";

const USAGE: &str = "\
ferry-sync — M0 walking skeleton

USAGE:
    ferry-sync genpoly [--seed N]
        Print a random irreducible chunker polynomial as 16 hex digits.

    ferry-sync daemon [--transport iroh|tcp] (default iroh)
                      --role listen|connect --store DIR --tree DIR
                      --tag NAME --poly HEX16

IROH TRANSPORT (default):
    ferry-sync daemon --role listen  [--relay URL]... [--discovery mdns]
                      [--force-relay] [--folder-id HEX32] [--poll-ms MS]
    ferry-sync daemon --role connect --peer HEX64 [--relay URL]... [...]
    Peers are addressed by their public endpoint id (derived from the
    device identity stored under <store>/.ferry/identity). Listeners print
    `ENDPOINT <hex64>`; connectors pass it back via --peer. Relays are
    operator-run (`ferry-relay` binary); without one, use --discovery mdns
    on shared LANs.

TCP TRANSPORT (--transport tcp; M0 throwaway, tests only):
    ferry-sync daemon --role listen  --addr HOST:PORT
    ferry-sync daemon --role connect --addr HOST:PORT

Roles: exactly one side runs `listen`; the other runs `connect` and drives
all sessions.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("genpoly") => cmd_genpoly(&args[1..]),
        Some("daemon") => cmd_daemon(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    out
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn require(args: &[String], name: &str) -> Result<String, String> {
    flag(args, name).ok_or(format!("missing required flag {name}"))
}

fn cmd_genpoly(args: &[String]) -> ExitCode {
    let seed: u64 = flag(args, "--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            use rand::RngCore;
            rand::rngs::OsRng.next_u64()
        });
    let poly = ferry_store::chunker::generate_polynomial(&mut StdRng::seed_from_u64(seed));
    println!("{poly:016x}");
    ExitCode::SUCCESS
}

fn cmd_daemon(args: &[String]) -> ExitCode {
    match parse_and_run_daemon(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

enum TransportKind {
    Iroh,
    Tcp,
}

struct DaemonArgs {
    kind: TransportKind,
    role: String,
    addr: Option<std::net::SocketAddr>,
    peer: Option<[u8; 32]>,
    relays: Vec<String>,
    mdns: bool,
    force_relay: bool,
    store_dir: std::path::PathBuf,
    tree_dir: std::path::PathBuf,
    tag: String,
    poly: u64,
    folder_id: [u8; 16],
    poll_ms: u64,
    opportunistic_every: u32,
}

fn parse_and_run_daemon(args: &[String]) -> Result<(), String> {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let kind = match flag(args, "--transport").as_deref() {
        None | Some("iroh") => TransportKind::Iroh,
        Some("tcp") => TransportKind::Tcp,
        Some(other) => return Err(format!("--transport must be iroh|tcp, got {other:?}")),
    };

    let store_dir = std::path::PathBuf::from(require(args, "--store")?);
    let parsed = DaemonArgs {
        kind,
        role: require(args, "--role")?,
        // TCP mode demands --addr; iroh mode ignores it (fixed labels below).
        addr: match flag(args, "--addr") {
            Some(a) => {
                Some(std::net::SocketAddr::from_str(&a).map_err(|e| format!("--addr: {e}"))?)
            }
            None => None,
        },
        peer: match flag(args, "--peer") {
            Some(h) => Some(
                unhex::<32>(&h.replace(' ', ""))
                    .ok_or("--peer expects 64 hex digits (a 32-byte endpoint id)")?,
            ),
            None => None,
        },
        relays: flags(args, "--relay"),
        mdns: has_flag(args, "--discovery-mdns"),
        force_relay: has_flag(args, "--force-relay"),
        store_dir: store_dir.clone(),
        tree_dir: std::path::PathBuf::from(require(args, "--tree")?),
        tag: require(args, "--tag")?,
        poly: u64::from_str_radix(require(args, "--poly")?.trim_start_matches("0x"), 16)
            .map_err(|e| format!("--poly expects 16 hex digits: {e}"))?,
        folder_id: match flag(args, "--folder-id") {
            Some(h) => unhex::<16>(&h).ok_or("--folder-id expects 32 hex digits")?,
            None => ferry_sync::DEFAULT_FOLDER_ID,
        },
        poll_ms: flag(args, "--poll-ms")
            .and_then(|s| s.parse().ok())
            .unwrap_or(200),
        opportunistic_every: flag(args, "--opportunistic-every")
            .and_then(|s| s.parse().ok())
            .unwrap_or(ferry_sync::engine::DEFAULT_OPPORTUNISTIC_EVERY),
    };
    run_daemon(parsed)
}

fn run_daemon(d: DaemonArgs) -> Result<(), String> {
    ferry_sync::proto::validate_tag(&d.tag).map_err(|e| format!("--tag: {e}"))?;
    if d.role != "listen" && d.role != "connect" {
        return Err(format!("--role must be listen|connect, got {:?}", d.role));
    }

    let mut cfg = EngineConfig {
        tag: d.tag.clone(),
        store_dir: d.store_dir.clone(),
        tree_dir: d.tree_dir.clone(),
        // T-02: validate the user-supplied --poly HERE, at config load. A
        // typo used to surface as a chunker .expect() panic mid-scan.
        poly: ferry_store::chunker::ValidatedPoly::new(d.poly)
            .map_err(|e| format!("--poly: {e}"))?,
        folder_id: d.folder_id,
        poll_interval: Duration::from_millis(d.poll_ms),
        opportunistic_every: d.opportunistic_every,
        bind_addr: None,
        connect_to: None,
        // Strict peer pinning arrives with T-007's pairing ritual; the
        // skeleton accepts whichever identity proves key possession.
        expected_peer_id: None,
        // Protocol v1 with encryption ON is the only production path; the
        // retired plaintext framing is reachable programmatically only.
        legacy_m0_proto: false,
        quiet: false,
    };

    let transport: Arc<dyn ferry_sync::Transport> = match d.kind {
        TransportKind::Tcp => {
            let addr = d.addr.ok_or("--addr is required in --transport tcp mode")?;
            match d.role.as_str() {
                "listen" => cfg.bind_addr = Some(addr),
                "connect" => cfg.connect_to = Some(addr),
                _ => unreachable!("validated above"),
            }
            Arc::new(ferry_sync::TcpTransport)
        }
        TransportKind::Iroh => {
            // Stable endpoint identity derived from THIS store's device
            // identity (ferry-crypto): restart-safe public addressing.
            // NOTE: lives in a SIBLING of the store's `.ferry/` — that
            // directory belongs to the store layout, and creating it early
            // would flip Store::create into Store::open on first run.
            let identity_root = d.store_dir.join(".device-identity");
            let device = crypto_identity::load_or_create(&identity_root)
                .map_err(|e| format!("device identity: {e}"))?;

            let mut builder = ferry_iroh::IrohConfig::builder().device_identity(&device);
            if !d.relays.is_empty() {
                builder = builder.relays(ferry_iroh::RelaySetting::Custom(d.relays.clone()));
            }
            if d.mdns {
                builder = builder.mdns(ferry_iroh::MdnsSetting {
                    service_name: "ferry-sync".into(),
                    advertise: true,
                });
            }
            if d.force_relay {
                builder = builder.force_relay(true);
            }
            let t = ferry_iroh::IrohTransport::new(builder.build())
                .map_err(|e| format!("iroh transport: {e}"))?;

            let endpoint_hex = hex(&t.endpoint_id());
            match d.role.as_str() {
                "listen" => {
                    cfg.bind_addr =
                        Some(std::net::SocketAddr::from_str(IROH_BIND_ALIAS).expect("constant"));
                }
                "connect" => {
                    let peer = d.peer.ok_or(
                        "--peer HEX64 is required to connect (it is \
                        the listener's ENDPOINT id)",
                    )?;
                    let alias = std::net::SocketAddr::from_str(IROH_DIAL_ALIAS).expect("constant");
                    t.with_route(alias, peer);
                    cfg.connect_to = Some(alias);
                }
                _ => unreachable!("validated above"),
            }
            // Announce AFTER wiring config so the listener's alias exists
            // before any STATE line; ordering with LISTENING is preserved.
            println!("ENDPOINT {endpoint_hex}");
            Arc::new(t)
        }
    };

    let engine = SyncEngine::new(cfg, transport).map_err(|e| format!("startup failed: {e}"))?;
    if let Some(a) = engine.listen_addr() {
        println!("LISTENING {a}");
    }
    // Run until killed; EngineHandle shutdown happens on drop.
    let handle = engine.start();
    handle.join_until_signal();
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}
