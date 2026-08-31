


























use std::process::ExitCode;
use std::str::FromStr as _;
use std::sync::Arc;
use std::time::Duration;

use rand::rngs::StdRng;
use rand::SeedableRng;

use ferry_crypto::identity as crypto_identity;
#[cfg(feature = "web-ui")]
use ferry_daemon::ui;
use ferry_store::format::{hex, unhex};
use ferry_sync::{EngineConfig, SyncEngine};


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

IPC SERVER:
    The daemon always runs an IPC server on the folder socket
    (<store>/.ferry/daemon.sock or --socket PATH) broadcasting live
    snapshots, engine state changes, transfers, and conflicts.

WEB DASHBOARD (optional):
    ferry-sync daemon ... [--ui [HOST:PORT]]   (default 127.0.0.1:8098)
    Loopback binds only, no auth (v0 stance); serves the embedded UI plus
    /api/status, /api/conflicts, /api/share, /api/pair/accept and
    /api/pin/start|stop|release per .scratch/web-dashboard/spec.md.

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

fn ferry_home() -> std::path::PathBuf {
    if let Some(v) = std::env::var_os("FERRY_HOME") {
        let p = std::path::PathBuf::from(&v);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(std::path::PathBuf::from)
    {
        if !home.as_os_str().is_empty() {
            return home.join(".ferry");
        }
    }
    std::path::PathBuf::from("/tmp/.ferry")
}

fn cmd_daemon(args: &[String]) -> ExitCode {
    
    
    let is_legacy = has_flag(args, "--store")
        || has_flag(args, "--tree")
        || has_flag(args, "--role")
        || has_flag(args, "--transport");
    if !is_legacy {
        match run_central_daemon(args) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    
    if has_flag(args, "--listen") {
        eprintln!(
            "warning: --listen is deprecated; use `ferry daemon` without args for device daemon"
        );
        
        if let Some(p) = flag(args, "--listen") {
            let home = ferry_home();
            let _ = std::fs::create_dir_all(&home);
            if let Ok(rec) = ferry_folder::inventory::FolderInventory::new(&home)
                .register(&std::path::PathBuf::from(&p))
            {
                eprintln!(
                    "registered folder {} -> {}",
                    rec.path.display(),
                    rec.folder_id
                );
            }
        }
    }
    match parse_and_run_daemon(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_central_daemon(args: &[String]) -> Result<(), String> {
    let home = ferry_home();
    let folders: Vec<std::path::PathBuf> = args
        .iter()
        .filter(|a| !a.starts_with('-') && a.as_str() != "daemon")
        .map(std::path::PathBuf::from)
        .collect();
    
    let identity = ferry_crypto::identity::load_or_create(&home.join("identity"))
        .or_else(|_| {
            ferry_crypto::identity::load_or_create(&home.join("identity").join("device.key"))
        })
        .map_err(|e| format!("device identity: {e}"))?;
    ferry_daemon::device_daemon::run(&home, identity, &folders).map_err(|e| e.to_string())
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
    ui_addr: Option<std::net::SocketAddr>,
    socket_path: Option<std::path::PathBuf>,
}



fn parse_ui_addr(args: &[String]) -> Result<Option<std::net::SocketAddr>, String> {
    if !has_flag(args, "--ui") {
        return Ok(None);
    }
    let raw = flag(args, "--ui").unwrap_or_else(|| "127.0.0.1:8098".to_string());
    let addr = std::net::SocketAddr::from_str(&raw).map_err(|e| format!("--ui {raw:?}: {e}"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "--ui {addr}: refusing non-loopback bind; the dashboard serves localhost only"
        ));
    }
    Ok(Some(addr))
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
        ui_addr: parse_ui_addr(args)?,
        socket_path: flag(args, "--socket").map(std::path::PathBuf::from),
    };
    run_daemon(parsed)
}

fn validate_tag(tag: &str) -> Result<(), String> {
    if tag.is_empty() || tag.len() > 64 || tag.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("tag must be 1..64 non-whitespace chars".to_string());
    }
    Ok(())
}

fn run_daemon(d: DaemonArgs) -> Result<(), String> {
    validate_tag(&d.tag).map_err(|e| format!("--tag: {e}"))?;
    if d.role != "listen" && d.role != "connect" {
        return Err(format!("--role must be listen|connect, got {:?}", d.role));
    }

    let mut cfg = EngineConfig {
        tag: d.tag.clone(),
        store_dir: d.store_dir.clone(),
        tree_dir: d.tree_dir.clone(),
        
        
        poly: ferry_store::chunker::ValidatedPoly::new(d.poly)
            .map_err(|e| format!("--poly: {e}"))?,
        folder_id: d.folder_id,
        poll_interval: Duration::from_millis(d.poll_ms),
        opportunistic_every: d.opportunistic_every,
        bind_addr: None,
        connect_to: None,
        
        
        allow_trust_on_first_use: false,
        
        
        
        pin_state_dir: Some(d.store_dir.join(".ferry")),
        quiet: false,
    };

    
    
    
    
    
    
    let device = crypto_identity::load_or_create(&d.store_dir.join(".device-identity"))
        .map_err(|e| format!("device identity: {e}"))?;

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
            let cfg_iroh = ferry_iroh::IrohConfig {
                device_identity: Some(device.clone()),
                relays: if d.relays.is_empty() {
                    ferry_iroh::RelaySetting::Disabled
                } else {
                    ferry_iroh::RelaySetting::Custom(d.relays.clone())
                },
                mdns: if d.mdns {
                    Some(ferry_iroh::MdnsSetting {
                        service_name: "ferry-sync".into(),
                        advertise: true,
                    })
                } else {
                    None
                },
                force_relay: d.force_relay,
                ..Default::default()
            };
            let t = ferry_iroh::IrohTransport::new(cfg_iroh)
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
                    let alias = t.register_peer(peer);
                    cfg.connect_to = Some(alias);
                }
                _ => unreachable!("validated above"),
            }
            
            
            println!("ENDPOINT {endpoint_hex}");
            Arc::new(t)
        }
    };

    
    
    
    
    
    let store: Arc<ferry_store::store::Store> =
        if ferry_folder::folder::dot_dir(&d.store_dir).is_dir() {
            ferry_folder::folder::open_folder(&d.store_dir, &device)
                .map_err(|e| format!("startup failed: {e}"))?
                .store
        } else {
            let (store, _fmk) =
                ferry_folder::folder::create_folder(&d.store_dir, &device, d.folder_id, d.poly)
                    .map_err(|e| format!("startup failed: {e}"))?;
            
            
            store
                .flush()
                .map_err(|e| format!("startup failed: flush: {e}"))?;
            store
                .write_index_snapshot()
                .map_err(|e| format!("startup failed: index snapshot: {e}"))?;
            Arc::new(store)
        };

    let mut engine = SyncEngine::with_store(cfg, transport, store)
        .map_err(|e| format!("startup failed: {e}"))?;
    engine.set_identity(device.clone());
    if let Some(a) = engine.listen_addr() {
        println!("LISTENING {a}");
    }
    
    let handle = engine.start();

    let (broadcast_tx, _) = tokio::sync::broadcast::channel(256);
    let daemon_state = Arc::new(ferry_daemon::state::DaemonState::new(
        handle.clone(),
        d.store_dir.clone(),
        d.tree_dir.clone(),
        d.folder_id,
        device.clone(),
        broadcast_tx,
    ));

    #[allow(deprecated)]
    let socket_path = d
        .socket_path
        .unwrap_or_else(|| ferry_ipc::paths::socket_path_for_dir(&d.store_dir));
    let ipc_handle =
        ferry_daemon::ipc::spawn_ipc_server(socket_path.clone(), Arc::clone(&daemon_state))
            .map_err(|e| format!("ipc server: {e}"))?;
    #[cfg(feature = "web-ui")]
    if let Some(addr) = d.ui_addr {
        let state = ui::UiState::new(handle.clone(), d.store_dir, d.tree_dir, d.folder_id, device);
        ui::spawn(addr, Arc::new(state)).map_err(|e| format!("--ui: {e}"))?;
    }
    #[cfg(not(feature = "web-ui"))]
    if d.ui_addr.is_some() {
        return Err(
            "web dashboard disabled in this build (compiled with --features lean)".to_string(),
        );
    }

    handle.join_until_signal();
    ipc_handle.shutdown();
    Ok(())
}
