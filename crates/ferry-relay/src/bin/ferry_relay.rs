//! `ferry-relay` CLI: one blind relay binary for a VPS.
//!
//! ```text
//! ferry-relay --http-bind 0.0.0.0:3340
//! ```
//!
//! Prints `RELAY <url>` once serving; clients pass that URL as their
//! `--relay`. Logs go to stderr and contain ONLY connection metadata:
//! client endpoint public keys, addresses, timing. No payload data is ever
//! readable or logged here — see crate docs.
//!
//! Production notes (docs/nat-validation.md has the full runbook): put this
//! behind real TLS (reverse proxy or the ACME support in iroh-relay) for
//! anything internet-facing; plain HTTP is fine on loopback/private nets.

use std::process::ExitCode;

const USAGE: &str = "\
ferry-relay — self-hostable blind relay for ferry-sync (ADR-0003)

USAGE:
    ferry-relay [--http-bind HOST:PORT]

The relay forwards opaque ciphertext between authenticated endpoints. It
sees: client public keys, IPs, ports, timing, byte counts — nothing else.

Default bind: 127.0.0.1:3340 (loopback; pass 0.0.0.0:PORT to serve).";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let bind: std::net::SocketAddr = match args.iter().position(|a| a == "--http-bind") {
        Some(i) => match args.get(i + 1) {
            Some(v) => match v.parse() {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("error: --http-bind: {e}");
                    return ExitCode::FAILURE;
                }
            },
            None => {
                eprintln!("error: --http-bind needs HOST:PORT");
                return ExitCode::FAILURE;
            }
        },
        None => "127.0.0.1:3340".parse().unwrap(),
    };

    // Operator-visible logging: metadata only, by construction.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    rt.block_on(async move {
        let relay = match ferry_relay::spawn(ferry_relay::RelayOptions::new(bind)).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e}");
                return;
            }
        };
        println!("RELAY {}", relay.url());
        println!("listening on {} (blind ciphertext pipe)", relay.http_addr());

        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        tokio::select! {
            _ = shutdown => {}
            _ = park_forever() => {}
        }
        relay.shutdown().await;
        eprintln!("relay shut down");
    });
    ExitCode::SUCCESS
}

async fn park_forever() {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    }
}
