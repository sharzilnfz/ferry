//! `ferry-sync` CLI: run one M0 daemon, or generate a folder polynomial.
//!
//! Daemon (two of these make a skeleton pair):
//!
//! ```text
//! ferry-sync daemon --role listen  --addr 127.0.0.1:41001 \
//!     --store /tmp/node-a/store --tree /tmp/node-a/tree --tag node-a --poly <hex16>
//! ferry-sync daemon --role connect --addr 127.0.0.1:41001 ... --tag node-b
//! ```
//!
//! The listener prints `LISTENING <addr>`; both print machine-greppable
//! `STATE root=<hex> agreed=<hex|none>` lines whenever they change. The
//! connector drives sessions; the listener serves them and is polled by the
//! connector's opportunistic dials.
//!
//! Status lines go to stdout; errors to stderr.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::SeedableRng;

use ferry_sync::{EngineConfig, SyncEngine};

const USAGE: &str = "\
ferry-sync — M0 walking skeleton

USAGE:
    ferry-sync genpoly [--seed N]
        Print a random irreducible chunker polynomial as 16 hex digits.

    ferry-sync daemon --role listen|connect --addr HOST:PORT
                      --store DIR --tree DIR --tag NAME --poly HEX16
                      [--folder-id HEX32] [--poll-ms MS] [--opportunistic-every N]

Roles: exactly one side runs `listen` (binds --addr); the other runs
`connect` (dials --addr) and drives all sessions.";

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

fn parse_and_run_daemon(args: &[String]) -> Result<(), String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return Ok(());
    }
    let role = require(args, "--role")?;
    let addr: std::net::SocketAddr = require(args, "--addr")?
        .parse()
        .map_err(|e| format!("--addr: {e}"))?;
    let store_dir = std::path::PathBuf::from(require(args, "--store")?);
    let tree_dir = std::path::PathBuf::from(require(args, "--tree")?);
    let tag = require(args, "--tag")?;
    let poly_hex = require(args, "--poly")?;
    let poly = u64::from_str_radix(poly_hex.trim_start_matches("0x"), 16)
        .map_err(|e| format!("--poly expects 16 hex digits: {e}"))?;
    let folder_id: [u8; 16] = match flag(args, "--folder-id") {
        Some(h) => {
            ferry_store::format::unhex::<16>(&h).ok_or("--folder-id expects 32 hex digits")?
        }
        None => ferry_sync::DEFAULT_FOLDER_ID,
    };
    let poll_ms: u64 = flag(args, "--poll-ms")
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let opportunistic_every: u32 = flag(args, "--opportunistic-every")
        .and_then(|s| s.parse().ok())
        .unwrap_or(ferry_sync::engine::DEFAULT_OPPORTUNISTIC_EVERY);

    ferry_sync::proto::validate_tag(&tag).map_err(|e| format!("--tag: {e}"))?;

    let mut cfg = EngineConfig {
        tag,
        store_dir,
        tree_dir,
        poly,
        folder_id,
        poll_interval: Duration::from_millis(poll_ms),
        opportunistic_every,
        bind_addr: None,
        connect_to: None,
        quiet: false,
    };
    match role.as_str() {
        "listen" => cfg.bind_addr = Some(addr),
        "connect" => cfg.connect_to = Some(addr),
        other => return Err(format!("--role must be listen|connect, got {other:?}")),
    }

    let transport = Arc::new(ferry_sync::TcpTransport);
    let engine = SyncEngine::new(cfg, transport).map_err(|e| format!("startup failed: {e}"))?;
    if let Some(a) = engine.listen_addr() {
        println!("LISTENING {a}");
    }
    // Run until killed; EngineHandle shutdown happens on drop.
    let handle = engine.start();
    handle.join_until_signal();
    Ok(())
}
